// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what the CONSUMERS of a vended log group see of a state machine's
//! delivered execution-history events, driven over each service's own wire protocol
//! through the production `MultiServiceRouter` with Step Functions, CloudWatch and
//! CloudWatch Logs mounted on ONE engine.
//!
//! The rule under test: a history event a `loggingConfiguration` delivers is an ORDINARY
//! INGESTION into the log group it names, so everything that log group is configured to do
//! with an ingested event is done with it — the group's METRIC FILTERS extract metric data
//! from it under their own namespaces and dimensions, and the group's own INGESTION
//! METRICS count it — exactly as they would for an event a client put there itself. That
//! is the whole reason a client points a metric filter, a subscription filter or a Logs
//! Insights query at a vended log group: without it, every log-derived signal built on
//! Step Functions execution logging reads zero.
//!
//! The rows differ along what the rule generalises over: the workflow type (EXPRESS and
//! STANDARD), the logging `level` (`ALL` and `ERROR`), the outcome the runs reach
//! (succeeded and failed), how many runs there are, the filter PATTERN (a JSON selector on
//! an event member, and the empty pattern that selects every event), the form of the
//! `metricValue` (a literal and a selector read out of the event) and whether the
//! transformation is dimensioned.
//!
//! Every expectation is COMPUTED from the events the group really holds, read back through
//! CloudWatch Logs' own reader: the assertion is that the filters extracted from exactly
//! the delivered events, not that a particular number was published.
//!
//! The harness (`Observed`, `create`, `logging`) is the one `execution_observability`
//! builds for the same three services, and the group's reader is
//! `vended_log_event_members`'; `tests/all.rs` is this binary's crate root, so both are
//! reached through `crate::<suite>`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;
use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::emf;
use service_com_amazonaws_sfn_awsstepfunctions::state::{observability, EXECUTION_SETTLE_DELAY};

use crate::execution_observability::{create, logging, Observed, FAILING_DEFINITION};
use crate::state_machine_lifecycle::{string, HELLO_DEFINITION};

/// A definition that enters one state and then fails, so one run carries an ordinary
/// progress event, a failure of a state and the failure of the execution.
const PASS_THEN_FAIL: &str = r#"{"StartAt":"Note","States":{
    "Note":{"Type":"Pass","Result":{"noted":true},"Next":"Nope"},
    "Nope":{"Type":"Fail","Error":"Boom","Cause":"the row fails on purpose"}}}"#;

/// How long a row waits for every run's events to reach the group before it fails.
///
/// A run's outcome is due one [`EXECUTION_SETTLE_DELAY`] after its start and the clock
/// that materialises it wakes on its own schedule, so the wait is that span with ample room
/// around it — far longer than it takes, and bounded so a failing row reports rather than
/// hangs.
const DELIVERY_DEADLINE: Duration = EXECUTION_SETTLE_DELAY.saturating_add(Duration::from_secs(60));

/// How often a row looks at the log group while it waits.
const POLL: Duration = Duration::from_millis(25);

/// The metric name every row publishes under, in its own namespace.
const METRIC: &str = "Extracted";

/// One row: the runs to log, the metric filter to put on the destination group, and the
/// row's own reading of which delivered events that filter selects and what each of them
/// publishes.
struct Row {
    /// What the row varies, named in every assertion it makes.
    case: &'static str,
    /// `STANDARD` or `EXPRESS`.
    sm_type: &'static str,
    /// The logging configuration's `level`.
    level: &'static str,
    /// The definition the runs interpret.
    definition: &'static str,
    /// How many executions the row starts.
    runs: usize,
    /// The metric filter's `filterPattern`.
    pattern: &'static str,
    /// The transformation's `metricValue`.
    metric_value: &'static str,
    /// The transformation's one `dimensions` entry, as `(name, selector)`, where the row
    /// declares one.
    dimension: Option<(&'static str, &'static str)>,
    /// Which delivered events the pattern selects, as the row reads them: the expectation
    /// is computed from the events the group really holds.
    selects: fn(&Value) -> bool,
    /// What one selected event publishes, read off that event.
    value_of: fn(&Value) -> f64,
    /// Whether this row's runs deliver any event the pattern selects at all, so a row that
    /// expects an extraction cannot pass by delivering nothing and a row that expects none
    /// cannot pass by selecting something.
    selects_any: bool,
}

/// Every event type is selected, which is what the empty pattern does.
fn every_event(_event: &Value) -> bool {
    true
}

/// Only the event that ends a failed execution.
fn execution_failed(event: &Value) -> bool {
    event.get("type").and_then(Value::as_str) == Some("ExecutionFailed")
}

/// Only the event that opens an execution.
fn execution_started(event: &Value) -> bool {
    event.get("type").and_then(Value::as_str) == Some("ExecutionStarted")
}

/// One, which is what a transformation stating a literal publishes.
fn one(_event: &Value) -> f64 {
    1.0
}

/// The event's own history id, which is what a transformation selecting `$.id` publishes.
///
/// A vended event carries its id as TEXT, so the number is read out of that text — the
/// same reading a filter's selector makes of it.
fn event_id(event: &Value) -> f64 {
    event
        .get("id")
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<f64>().ok())
        .unwrap_or_default()
}

/// The `dimensions` map of a transformation, from one `(name, selector)` entry.
fn transformation_dimensions(entry: (&str, &str)) -> Value {
    json!({ entry.0: entry.1 })
}

/// What the selector `$.<member>` resolves to in one delivered event, which is the value
/// the dimension of that selector is published under.
fn selected_text(event: &Value, selector: &str, case: &str) -> String {
    let member = selector
        .strip_prefix("$.")
        .unwrap_or_else(|| panic!("case {case:?}: a row's selector names a member, got {selector}"));
    event
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a delivered event to carry {member} as text, got {event}")
        })
        .to_string()
}

/// Every datapoint the CloudWatch metric store holds for one metric, summed per dimension
/// set.
///
/// This reads the SAME store `PutMetricData` writes and `GetMetricStatistics` answers
/// from, through the shared engine the three services are mounted on.
fn published(
    observed: &Observed,
    namespace: &str,
    metric_name: &str,
) -> BTreeMap<BTreeMap<String, String>, f64> {
    let state = cloudwatch::service_state(&**observed.engine())
        .expect("the CloudWatch metric store is mounted on the shared engine");
    let store = state.lock();
    let mut summed: BTreeMap<BTreeMap<String, String>, f64> = BTreeMap::new();
    for (key, data) in store.metrics.iter() {
        if key.namespace != namespace || key.metric_name != metric_name {
            continue;
        }
        for datum in data {
            *summed.entry(key.dimensions.clone()).or_default() += datum.sum;
        }
    }
    summed
}

/// The events `group` holds, waited for until every one of `runs` executions has delivered
/// its own.
///
/// A delivery owed to the clock has not landed when the start answers, so the wait is for
/// the CONDITION — events naming `runs` distinct executions — rather than for a span.
async fn delivered_runs(observed: &Observed, group: &str, case: &str, runs: usize) -> Vec<Value> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let messages = crate::vended_log_event_members::delivered(observed, case, group).await;
        let executions: BTreeSet<&str> = messages
            .iter()
            .filter_map(|message| message.get("execution_arn").and_then(Value::as_str))
            .collect();
        if executions.len() >= runs {
            return messages;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the events of {runs} run(s) in {group} within \
             {DELIVERY_DEADLINE:?}, got {} execution(s) in {messages:?}",
            executions.len()
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// A metric filter on the destination log group extracts metric data from the history
/// events a state machine's logging configuration delivers, under the namespace,
/// dimensions and value its own transformation declares.
#[tokio::test]
async fn a_metric_filter_extracts_from_the_delivered_history_events() {
    let observed = Observed::new();

    let rows: Vec<Row> = vec![
        Row {
            case: "an EXPRESS failure counter counts one per failed run",
            sm_type: "EXPRESS",
            level: observability::LOG_LEVEL_ALL,
            definition: FAILING_DEFINITION,
            runs: 3,
            pattern: r#"{$.type = "ExecutionFailed"}"#,
            metric_value: "1",
            dimension: None,
            selects: execution_failed,
            value_of: one,
            selects_any: true,
        },
        Row {
            case: "the same counter over runs that succeed publishes nothing",
            sm_type: "EXPRESS",
            level: observability::LOG_LEVEL_ALL,
            definition: HELLO_DEFINITION,
            runs: 2,
            pattern: r#"{$.type = "ExecutionFailed"}"#,
            metric_value: "1",
            dimension: None,
            selects: execution_failed,
            value_of: one,
            selects_any: false,
        },
        Row {
            case: "a counter dimensioned on the execution is one series per run",
            sm_type: "EXPRESS",
            level: observability::LOG_LEVEL_ALL,
            definition: FAILING_DEFINITION,
            runs: 2,
            pattern: r#"{$.type = "ExecutionFailed"}"#,
            metric_value: "1",
            dimension: Some(("ExecutionArn", "$.execution_arn")),
            selects: execution_failed,
            value_of: one,
            selects_any: true,
        },
        Row {
            case: "a value selected out of the event publishes that value",
            sm_type: "STANDARD",
            level: observability::LOG_LEVEL_ALL,
            definition: PASS_THEN_FAIL,
            runs: 1,
            pattern: r#"{$.type = "ExecutionStarted"}"#,
            metric_value: "$.id",
            dimension: None,
            selects: execution_started,
            value_of: event_id,
            selects_any: true,
        },
        Row {
            case: "the empty pattern counts every delivered event of a STANDARD run",
            sm_type: "STANDARD",
            level: observability::LOG_LEVEL_ALL,
            definition: PASS_THEN_FAIL,
            runs: 1,
            pattern: "",
            metric_value: "1",
            dimension: None,
            selects: every_event,
            value_of: one,
            selects_any: true,
        },
        Row {
            case: "an ERROR level delivers only the failures, so only those are counted",
            sm_type: "EXPRESS",
            level: observability::LOG_LEVEL_ERROR,
            definition: PASS_THEN_FAIL,
            runs: 2,
            pattern: "",
            metric_value: "1",
            dimension: None,
            selects: every_event,
            value_of: one,
            selects_any: true,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let group = format!("/aws/vendedlogs/states/ingest-{index}");
        let namespace = format!("Custom/SfnVendedIngest{index}");
        // The group and its metric filter are the account's, in place before the state
        // machine logs into it — exactly as a client configuring the signal sets it up.
        observed
            .logs("CreateLogGroup", &json!({ "logGroupName": group }))
            .await;
        let mut transformation = json!({
            "metricName": METRIC,
            "metricNamespace": &namespace,
            "metricValue": row.metric_value,
        });
        if let Some(entry) = row.dimension {
            transformation["dimensions"] = transformation_dimensions(entry);
        }
        observed
            .logs(
                "PutMetricFilter",
                &json!({
                    "logGroupName": &group,
                    "filterName": "extract",
                    "filterPattern": row.pattern,
                    "metricTransformations": [transformation],
                }),
            )
            .await;

        let machine = create(
            &observed,
            &format!("ingest-{index}"),
            row.definition,
            row.sm_type,
            Some(logging(row.level, &group, true)),
        )
        .await;
        let mut started: Vec<String> = Vec::with_capacity(row.runs);
        for run in 0..row.runs {
            let answered = observed
                .ok(
                    "StartExecution",
                    &json!({
                        "stateMachineArn": machine,
                        "name": format!("run-{run}"),
                        "input": format!("{{\"run\":{run}}}"),
                    }),
                )
                .await;
            started.push(string(&answered, "executionArn"));
        }

        // What the group really holds, read back through CloudWatch Logs' own reader: the
        // expectation below is computed from these events and nothing else.
        let messages = delivered_runs(&observed, &group, case, row.runs).await;
        let executions: BTreeSet<&str> = messages
            .iter()
            .filter_map(|message| message.get("execution_arn").and_then(Value::as_str))
            .collect();
        for execution in &started {
            assert!(
                executions.contains(execution.as_str()),
                "case {case:?}: expected every run's events in the group, got {executions:?}"
            );
        }

        let mut expected: BTreeMap<BTreeMap<String, String>, f64> = BTreeMap::new();
        for message in &messages {
            if !(row.selects)(message) {
                continue;
            }
            let mut dimensions: BTreeMap<String, String> = BTreeMap::new();
            if let Some((name, selector)) = row.dimension {
                dimensions.insert(name.to_string(), selected_text(message, selector, case));
            }
            *expected.entry(dimensions).or_default() += (row.value_of)(message);
        }
        let actual = published(&observed, &namespace, METRIC);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the filter to have extracted {expected:?} from the \
             {} delivered event(s), got {actual:?}",
            messages.len()
        );
        // A row that expects an extraction must really have delivered something the
        // pattern selects, and a row that expects none must really have delivered nothing.
        let selected = messages
            .iter()
            .filter(|message| (row.selects)(message))
            .count();
        assert_eq!(
            selected > 0,
            row.selects_any,
            "case {case:?}: expected selects_any={}, got {selected} selected of {} delivered \
             event(s)",
            row.selects_any,
            messages.len()
        );
    }
}

/// The destination log group's OWN ingestion metrics count what a logging configuration
/// delivered into it: a vended delivery is an ingestion, so the group reports having taken
/// those events in.
#[tokio::test]
async fn the_destination_groups_ingestion_metrics_count_the_delivered_events() {
    let observed = Observed::new();
    let group = "/aws/vendedlogs/states/ingest-metrics";
    observed
        .logs("CreateLogGroup", &json!({ "logGroupName": group }))
        .await;
    let machine = create(
        &observed,
        "ingest-metrics",
        PASS_THEN_FAIL,
        "EXPRESS",
        Some(logging(observability::LOG_LEVEL_ALL, group, true)),
    )
    .await;
    const RUNS: usize = 2;
    for run in 0..RUNS {
        observed
            .ok(
                "StartExecution",
                &json!({
                    "stateMachineArn": machine,
                    "name": format!("run-{run}"),
                    "input": format!("{{\"run\":{run}}}"),
                }),
            )
            .await;
    }
    let messages = delivered_runs(&observed, group, "the group's own ingestion", RUNS).await;
    // What the group took IN is counted off the messages as they are STORED, so the bytes
    // are read off the raw messages CloudWatch Logs reports rather than off a re-encoding
    // of them.
    let raw = raw_messages(&observed, group).await;
    assert_eq!(
        raw.len(),
        messages.len(),
        "expected one raw message per delivered event, got {} for {}",
        raw.len(),
        messages.len()
    );
    let dimensions: BTreeMap<String, String> = BTreeMap::from([(
        emf::LOG_GROUP_NAME_DIMENSION.to_string(),
        group.to_string(),
    )]);

    // (the metric, what the delivered events weigh in it)
    #[allow(clippy::cast_precision_loss)]
    let rows = [
        (emf::INCOMING_LOG_EVENTS_METRIC, raw.len() as f64),
        (
            emf::INCOMING_BYTES_METRIC,
            raw.iter().map(String::len).sum::<usize>() as f64,
        ),
    ];
    for (metric, expected) in rows {
        let actual = published(&observed, emf::LOGS_NAMESPACE, metric)
            .get(&dimensions)
            .copied();
        assert_eq!(
            actual,
            Some(expected),
            "case {metric:?}: expected the group to report having taken in {expected} for the \
             {} delivered event(s), got {actual:?}",
            raw.len()
        );
    }
}

/// The messages `group` holds, exactly as CloudWatch Logs reports them.
async fn raw_messages(observed: &Observed, group: &str) -> Vec<String> {
    let found = observed
        .logs("FilterLogEvents", &json!({ "logGroupName": group }))
        .await;
    found
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event.get("message").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}
