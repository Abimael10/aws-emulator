// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what an execution PUBLISHES about itself, driven over each
//! service's own wire protocol through the production `MultiServiceRouter` with Step
//! Functions, CloudWatch and CloudWatch Logs mounted on ONE engine — the shape
//! `core/router/src/lib.rs` mounts them in — so the metrics and the log events land in
//! the stores those services' own readers answer from.
//!
//! The rules under test:
//!
//! 1. an execution is counted in the `AWS/States` series dimensioned on its state
//!    machine: one datapoint per execution STARTED, one under the series for the
//!    terminal status it reached, and the span it ran for;
//! 2. an EXPRESS workflow, which is billed per run, also publishes the two billed
//!    series, whose values are the figures `StartSyncExecution` reports as
//!    `billingDetails` — the duration in milliseconds and the memory in bytes, so a
//!    window summed over either equals the summed billing details of the runs in it —
//!    beside the memory each run consumed, one sample per run, and a STANDARD workflow
//!    publishes none of the three;
//! 3. a state machine whose `loggingConfiguration` names a log group delivers its
//!    executions' history events to that group, with the events selected by `level`
//!    and the execution data included only where `includeExecutionData` asks for it.
//!
//! The rows differ along what the rules generalise over: the outcome an execution
//! reaches (succeeded, failed, aborted by a stop), the workflow type (STANDARD and
//! EXPRESS, synchronous and not) and the logging level (`ALL`, `ERROR`, `FATAL`,
//! `OFF`, and a state machine configured with no logging at all).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;
use service_com_amazonaws_cloudwatchlogs_logs_20140328::state as logs;
use service_com_amazonaws_sfn_awsstepfunctions::state::observability;
use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::state_machine_lifecycle::{
    error_code, string, ACCOUNT, HELLO_DEFINITION, PAST_THE_EXECUTION_DELAY, REGION, ROLE_ARN,
};

/// A definition that fails at once, so a row can observe a FAILED execution.
pub(crate) const FAILING_DEFINITION: &str = r#"{"StartAt":"Nope","States":{"Nope":{"Type":"Fail","Error":"Boom","Cause":"the row fails on purpose"}}}"#;

/// A definition that answers its own input, so what a run retains — and so the memory
/// it consumed — grows with the data it was handed.
const ECHO_DEFINITION: &str = r#"{"StartAt":"Echo","States":{"Echo":{"Type":"Pass","End":true}}}"#;

/// A definition that waits long enough to still be RUNNING when a stop arrives.
const WAITING_DEFINITION: &str =
    r#"{"StartAt":"Hold","States":{"Hold":{"Type":"Wait","Seconds":30,"Next":"Done"},"Done":{"Type":"Succeed"}}}"#;

/// Step Functions, CloudWatch and CloudWatch Logs over ONE engine.
///
/// `pub(crate)`, not private: `tests/all.rs` is this binary's crate root, so a sibling
/// suite that has to read what an execution DELIVERED mounts the three services through
/// this rather than carrying a second copy of the setup.
pub(crate) struct Observed {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Observed {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        Self { router, engine }
    }

    /// Call one Step Functions operation and return `(status, body)`.
    pub(crate) async fn call(&self, operation: &str, body: &Value) -> (u16, Value) {
        self.dispatch("states", "AWSStepFunctions", "1.0", operation, body)
            .await
    }

    /// Call one Step Functions operation, requiring a 200.
    pub(crate) async fn ok(&self, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "case {operation}: expected 200, got {status} with {parsed:?}"
        );
        parsed
    }

    /// Call one CloudWatch Logs operation, requiring a 200.
    pub(crate) async fn logs(&self, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self
            .dispatch("logs", "Logs_20140328", "1.1", operation, body)
            .await;
        assert_eq!(
            status, 200,
            "case {operation}: expected 200 from CloudWatch Logs, got {status} with {parsed:?}"
        );
        parsed
    }

    async fn dispatch(
        &self,
        signing_name: &str,
        target_prefix: &str,
        json_version: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("content-type", format!("application/x-amz-json-{json_version}"))
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
                ),
            )
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!(
                    "case {operation}: expected a JSON body, got {error} for {:?}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        (status, parsed)
    }

    /// The datapoints the series `metric` holds for `state_machine_arn`, read out of
    /// the CloudWatch store through the shared engine — the store CloudWatch's own
    /// readers answer from.
    fn series(&self, metric: &str, state_machine_arn: &str) -> Vec<cloudwatch::StoredDatum> {
        let store = cloudwatch::service_state(&*self.engine).expect("the CloudWatch store");
        let store = store.lock();
        store
            .metrics
            .get(&cloudwatch::MetricKey {
                namespace: observability::METRIC_NAMESPACE.to_string(),
                metric_name: metric.to_string(),
                dimensions: observability::state_machine_dimensions(state_machine_arn),
            })
            .cloned()
            .unwrap_or_default()
    }

    /// Whether the CloudWatch Logs store holds the group `name`.
    fn holds_log_group(&self, name: &str) -> bool {
        let store = logs::service_state(&*self.engine).expect("the CloudWatch Logs store");
        let holds = store.lock().log_groups.contains_key(name);
        holds
    }

    /// The engine the three services are mounted on.
    ///
    /// `pub(crate)`, not private: a sibling suite that reads a series this suite does not
    /// name — one a CloudWatch Logs metric filter published, in its own namespace and
    /// under its own dimensions — reads the store through the shared engine rather than
    /// mounting a second copy of this router.
    pub(crate) fn engine(&self) -> &Arc<engine::InMemoryEngine> {
        &self.engine
    }
}

/// The qualified ARN of the log group `name`, which is the form `logGroupArn` takes.
pub(crate) fn log_group_arn(name: &str) -> String {
    format!("arn:aws:logs:{REGION}:{ACCOUNT}:log-group:{name}:*")
}

/// A `loggingConfiguration` delivering to `name` at `level`.
pub(crate) fn logging(level: &str, name: &str, include_execution_data: bool) -> Value {
    json!({
        "level": level,
        "includeExecutionData": include_execution_data,
        "destinations": [{ "cloudWatchLogsLogGroup": { "logGroupArn": log_group_arn(name) } }],
    })
}

/// Create a state machine, answering its ARN.
pub(crate) async fn create(
    observed: &Observed,
    name: &str,
    definition: &str,
    sm_type: &str,
    logging_configuration: Option<Value>,
) -> String {
    let mut request = json!({
        "name": name,
        "definition": definition,
        "roleArn": ROLE_ARN,
        "type": sm_type,
    });
    if let Some(configuration) = logging_configuration {
        request["loggingConfiguration"] = configuration;
    }
    let created = observed.ok("CreateStateMachine", &request).await;
    string(&created, "stateMachineArn")
}

/// The sum of a series' datapoints, which is what a reader summing a window gets.
fn sum(datapoints: &[cloudwatch::StoredDatum]) -> f64 {
    datapoints.iter().map(|datum| datum.sum).sum()
}

/// How long a poll below waits for an outcome the clock owes: generous beside the span
/// this service settles one in, because what the test waits for is the CONDITION and
/// not an elapsed time.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll below waits between two looks at the store.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Poll `DescribeExecution` until `execution` reports a status other than `RUNNING`,
/// and answer that record — the call that materialises the owed outcome is also the
/// call that publishes what the execution owes its series. Fails with `case` when
/// [`TERMINAL_DEADLINE`] passes first.
async fn poll_until_terminal(observed: &Observed, case: &str, execution: &str) -> Value {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let described = observed
            .ok("DescribeExecution", &json!({ "executionArn": execution }))
            .await;
        if string(&described, "status") != "RUNNING" {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the execution to reach a terminal status, got {described:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Every execution is counted in the `AWS/States` series of its state machine: one
/// datapoint for the start, one under the series of the terminal status it reached, and
/// the span it ran for.
///
/// The rows differ along the outcome the execution reaches, which is what selects the
/// completion series, and they pin that NO other completion series is written.
#[tokio::test]
async fn an_execution_is_counted_under_its_state_machine() {
    let observed = Observed::new();

    // (case, the definition, how the row ends the execution, the series it must count in)
    let rows: Vec<(&str, &str, bool, &str)> = vec![
        (
            "an execution that succeeds",
            HELLO_DEFINITION,
            false,
            observability::EXECUTIONS_SUCCEEDED_METRIC,
        ),
        (
            "an execution that fails",
            FAILING_DEFINITION,
            false,
            observability::EXECUTIONS_FAILED_METRIC,
        ),
        (
            "an execution a stop aborts",
            WAITING_DEFINITION,
            true,
            observability::EXECUTIONS_ABORTED_METRIC,
        ),
    ];

    for (index, (case, definition, stopped, expected)) in rows.into_iter().enumerate() {
        let machine = create(
            &observed,
            &format!("counted-{index}"),
            definition,
            "STANDARD",
            None,
        )
        .await;
        let started = observed
            .ok(
                "StartExecution",
                &json!({ "stateMachineArn": machine, "name": "run" }),
            )
            .await;
        let execution = string(&started, "executionArn");

        // The start is counted at once, before the outcome is owed.
        let starts = observed.series(observability::EXECUTIONS_STARTED_METRIC, &machine);
        assert_eq!(
            sum(&starts),
            1.0,
            "case {case:?}: expected one started execution to be counted, got {starts:?}"
        );

        if stopped {
            observed
                .ok("StopExecution", &json!({ "executionArn": execution }))
                .await;
        } else {
            // The call that materialises the owed outcome is also the call that
            // publishes what the execution owes the series, so the poll waits for the
            // status rather than for a span to pass.
            poll_until_terminal(&observed, case, &execution).await;
        }

        for metric in [
            observability::EXECUTIONS_SUCCEEDED_METRIC,
            observability::EXECUTIONS_FAILED_METRIC,
            observability::EXECUTIONS_ABORTED_METRIC,
            observability::EXECUTIONS_TIMED_OUT_METRIC,
        ] {
            let held = observed.series(metric, &machine);
            let counted = sum(&held);
            let wanted = if metric == expected { 1.0 } else { 0.0 };
            assert_eq!(
                counted, wanted,
                "case {case:?}: expected {metric} to count {wanted}, got {counted} from {held:?}"
            );
        }
        let ran = observed.series(observability::EXECUTION_TIME_METRIC, &machine);
        assert_eq!(
            ran.len(),
            1,
            "case {case:?}: expected one execution-time datapoint, got {ran:?}"
        );
        assert_eq!(
            ran[0].unit.as_deref(),
            Some("Milliseconds"),
            "case {case:?}: an execution's duration is a span of milliseconds, got {ran:?}"
        );
        // A STANDARD workflow is not billed per run, so it publishes none of the
        // Express series.
        for metric in [
            observability::EXPRESS_BILLED_DURATION_METRIC,
            observability::EXPRESS_BILLED_MEMORY_METRIC,
            observability::EXPRESS_MEMORY_METRIC,
        ] {
            let held = observed.series(metric, &machine);
            assert!(
                held.is_empty(),
                "case {case:?}: a STANDARD workflow publishes no {metric}, got {held:?}"
            );
        }
    }
}

/// The bytes one billed megabyte of Express memory is reported as, which is the scale
/// the byte series carry the `billedMemoryUsedInMB` of a run's `billingDetails` at.
fn billed_megabytes_in_bytes(megabytes: f64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let scale = observability::BYTES_PER_BILLED_MEGABYTE as f64;
    megabytes * scale
}

/// Every EXPRESS run publishes the three Express series under its state machine's
/// dimension: the two BILLED series carrying exactly the figures that run reported as
/// `billingDetails` — the duration in milliseconds, and the memory in bytes at
/// [`observability::BYTES_PER_BILLED_MEGABYTE`] bytes per billed megabyte — and one
/// sample of the memory the run actually consumed.
///
/// The rows differ along what the rule generalises over: the shape and SIZE of the data
/// the run retains (no input at all, the empty object, a small object, a large one, a
/// list) and the outcome the run reaches (succeeded and failed). Each row runs on its
/// OWN state machine, so the series hold exactly that run's datapoint and the published
/// value can be compared against the figures that very call reported.
#[tokio::test]
async fn an_express_run_publishes_the_three_express_series() {
    let observed = Observed::new();
    let wide = json!({ "payload": "x".repeat(100_000) }).to_string();
    let listed = json!((0..64).collect::<Vec<u32>>()).to_string();

    // (case, the definition the run walks, the `input` it is handed)
    let rows: Vec<(&str, &str, Option<String>)> = vec![
        ("a run handed no input at all", HELLO_DEFINITION, None),
        (
            "a run handed the empty object",
            ECHO_DEFINITION,
            Some("{}".to_string()),
        ),
        (
            "a run handed a small object",
            ECHO_DEFINITION,
            Some(json!({ "wait": 2 }).to_string()),
        ),
        (
            "a run handed a large object",
            ECHO_DEFINITION,
            Some(wide),
        ),
        ("a run handed a list", ECHO_DEFINITION, Some(listed)),
        (
            "a run that fails",
            FAILING_DEFINITION,
            Some(json!({ "wait": 1 }).to_string()),
        ),
    ];

    // (case, the memory the run consumed) of every row, so the samples can be compared
    // against each other once the rows have run.
    let mut samples: Vec<(&str, f64)> = Vec::new();
    for (index, (case, definition, input)) in rows.into_iter().enumerate() {
        let machine = create(
            &observed,
            &format!("express-series-{index}"),
            definition,
            "EXPRESS",
            None,
        )
        .await;
        let mut request = json!({ "stateMachineArn": machine, "name": "run" });
        if let Some(input) = input {
            request["input"] = Value::String(input);
        }
        let answer = observed.ok("StartSyncExecution", &request).await;
        let details = answer.get("billingDetails").cloned().unwrap_or_else(|| {
            panic!("case {case:?}: a synchronous run reports its billing, got {answer:?}")
        });
        let reported = |member: &str| {
            details
                .get(member)
                .and_then(Value::as_f64)
                .unwrap_or_else(|| panic!("case {case:?}: expected {member}, got {details:?}"))
        };
        let billed_bytes = billed_megabytes_in_bytes(reported("billedMemoryUsedInMB"));

        // (the series, the value it must carry, the unit it must carry)
        let expected: Vec<(&str, f64, &str)> = vec![
            (
                observability::EXPRESS_BILLED_DURATION_METRIC,
                reported("billedDurationInMilliseconds"),
                observability::MILLISECONDS_UNIT,
            ),
            (
                observability::EXPRESS_BILLED_MEMORY_METRIC,
                billed_bytes,
                observability::BYTES_UNIT,
            ),
        ];
        for (metric, value, unit) in expected {
            let held = observed.series(metric, &machine);
            assert_eq!(
                held.len(),
                1,
                "case {case:?}: expected one {metric} datapoint, got {held:?}"
            );
            assert_eq!(
                held[0].sum, value,
                "case {case:?}: expected {metric} to carry the reported {value}, got {}",
                held[0].sum
            );
            assert_eq!(
                held[0].unit.as_deref(),
                Some(unit),
                "case {case:?}: expected {metric} in {unit}, got {held:?}"
            );
        }

        // The memory the run CONSUMED is its own sample, in the same bytes, and is what
        // the billed increment above was rounded up from — so it is never more than it.
        let consumed = observed.series(observability::EXPRESS_MEMORY_METRIC, &machine);
        assert_eq!(
            consumed.len(),
            1,
            "case {case:?}: expected one sample of the memory the run consumed, got {consumed:?}"
        );
        assert_eq!(
            consumed[0].unit.as_deref(),
            Some(observability::BYTES_UNIT),
            "case {case:?}: expected the consumed memory in bytes, got {consumed:?}"
        );
        assert!(
            consumed[0].sum > 0.0 && consumed[0].sum < billed_bytes,
            "case {case:?}: expected the consumed memory inside the billed {billed_bytes} bytes, \
             got {}",
            consumed[0].sum
        );
        samples.push((case, consumed[0].sum));
    }

    // The consumed series is the memory the run itself retained and not a second copy of
    // the billed increment: the row handed a large object retains the bytes of that
    // object, so its sample is strictly larger than the row handed the empty one, while
    // both are billed the same increment.
    let sample_of = |wanted: &str| {
        samples
            .iter()
            .find(|(case, _)| *case == wanted)
            .map(|(_, sample)| *sample)
            .unwrap_or_else(|| panic!("case {wanted:?}: expected a sample, got {samples:?}"))
    };
    let empty = sample_of("a run handed the empty object");
    let large = sample_of("a run handed a large object");
    assert!(
        large > empty,
        "case the consumed memory grows with the data retained: expected the large row's \
         {large} to exceed the empty row's {empty}"
    );
}

/// An EXPRESS workflow's billed series carry exactly the figures its runs report as
/// `billingDetails`, so the sum over the series equals the summed billing details, and
/// the memory series holds one sample per run; its STANDARD twin, run the same way,
/// publishes none of the three.
///
/// The runs differ along the data each is handed, so the sums are over figures that are
/// not all the same value.
#[tokio::test]
async fn express_billed_series_equal_the_summed_billing_details() {
    let observed = Observed::new();
    let express = create(&observed, "billed-express", ECHO_DEFINITION, "EXPRESS", None).await;
    let standard = create(
        &observed,
        "billed-standard",
        ECHO_DEFINITION,
        "STANDARD",
        None,
    )
    .await;

    // Three runs, so the series are a SUM over several datapoints rather than one, each
    // handed data of its own size.
    let inputs = [
        "{}".to_string(),
        json!({ "wait": 1 }).to_string(),
        json!({ "wait": 2, "payload": "y".repeat(10_000) }).to_string(),
    ];
    let mut billed_duration = 0.0;
    let mut billed_megabytes = 0.0;
    for (run, input) in inputs.iter().enumerate() {
        let answer = observed
            .ok(
                "StartSyncExecution",
                &json!({ "stateMachineArn": express, "name": format!("run-{run}"), "input": input }),
            )
            .await;
        let details = answer
            .get("billingDetails")
            .cloned()
            .unwrap_or_else(|| panic!("case run {run}: a synchronous run reports its billing"));
        billed_duration += details
            .get("billedDurationInMilliseconds")
            .and_then(Value::as_f64)
            .unwrap_or_default();
        billed_megabytes += details
            .get("billedMemoryUsedInMB")
            .and_then(Value::as_f64)
            .unwrap_or_default();
    }

    for (metric, reported) in [
        (
            observability::EXPRESS_BILLED_DURATION_METRIC,
            billed_duration,
        ),
        (
            observability::EXPRESS_BILLED_MEMORY_METRIC,
            billed_megabytes_in_bytes(billed_megabytes),
        ),
    ] {
        let held = observed.series(metric, &express);
        assert_eq!(
            held.len(),
            3,
            "case {metric}: expected one datapoint per run, got {held:?}"
        );
        assert_eq!(
            sum(&held),
            reported,
            "case {metric}: expected the summed billing details {reported}, got {}",
            sum(&held)
        );
    }
    let consumed = observed.series(observability::EXPRESS_MEMORY_METRIC, &express);
    assert_eq!(
        consumed.len(),
        3,
        "case {}: expected one sample per run, got {consumed:?}",
        observability::EXPRESS_MEMORY_METRIC
    );

    // The STANDARD twin: run the same definition the same number of times, and no
    // Express series exists for it at all.
    for (run, input) in inputs.iter().enumerate() {
        observed
            .ok(
                "StartExecution",
                &json!({ "stateMachineArn": standard, "name": format!("run-{run}"), "input": input }),
            )
            .await;
    }
    // Each twin run is polled to its own terminal status, so every datapoint the
    // STANDARD runs owe has been published by the time the series are read.
    let listed = observed
        .ok("ListExecutions", &json!({ "stateMachineArn": standard }))
        .await;
    let executions = listed
        .get("executions")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case the STANDARD twin: expected a listing, got {listed:?}"))
        .clone();
    assert_eq!(
        executions.len(),
        inputs.len(),
        "case the STANDARD twin: expected one execution per run, got {executions:?}"
    );
    for execution in &executions {
        let arn = string(execution, "executionArn");
        poll_until_terminal(&observed, "the STANDARD twin", &arn).await;
    }
    for metric in [
        observability::EXPRESS_BILLED_DURATION_METRIC,
        observability::EXPRESS_BILLED_MEMORY_METRIC,
        observability::EXPRESS_MEMORY_METRIC,
    ] {
        let held = observed.series(metric, &standard);
        assert!(
            held.is_empty(),
            "case {metric}: the STANDARD twin has none, got {held:?}"
        );
    }
    // Both twins are still counted as executions, so the absence above is about the
    // EXPRESS series and not about publishing at all.
    for machine in [&express, &standard] {
        let started = observed.series(observability::EXECUTIONS_STARTED_METRIC, machine);
        assert_eq!(
            sum(&started),
            3.0,
            "case {machine}: expected three started executions, got {started:?}"
        );
    }
}

/// The `level` of a logging configuration selects WHICH history events reach the log
/// group, and a configuration that logs nothing delivers nothing.
///
/// The rows differ along the level and along the outcome of the run, so each level is
/// seen against both an ordinary progress event and an execution-level failure.
#[tokio::test]
async fn a_logging_level_selects_the_events_delivered() {
    let observed = Observed::new();

    // (case, the level, the event types the group must hold for a FAILED run)
    let rows: Vec<(&str, Option<&str>, Vec<&str>)> = vec![
        (
            "ALL logs every event of the run",
            Some(observability::LOG_LEVEL_ALL),
            vec!["ExecutionStarted", "FailStateEntered", "ExecutionFailed"],
        ),
        (
            // A `Fail` state IS the failure, so entering it is an event of that tier;
            // the ordinary progress around it is not.
            "ERROR logs the failures alone",
            Some(observability::LOG_LEVEL_ERROR),
            vec!["FailStateEntered", "ExecutionFailed"],
        ),
        (
            "FATAL logs the execution-level failures",
            Some(observability::LOG_LEVEL_FATAL),
            vec!["ExecutionFailed"],
        ),
        ("OFF logs nothing", Some("OFF"), Vec::new()),
        ("no logging configuration logs nothing", None, Vec::new()),
    ];

    for (index, (case, level, expected)) in rows.into_iter().enumerate() {
        let group = format!("/aws/vendedlogs/states/level-{index}");
        let configuration = level.map(|level| logging(level, &group, true));
        let machine = create(
            &observed,
            &format!("level-{index}"),
            FAILING_DEFINITION,
            "STANDARD",
            configuration,
        )
        .await;
        let started = observed
            .ok(
                "StartExecution",
                &json!({ "stateMachineArn": machine, "name": "run" }),
            )
            .await;
        let execution = string(&started, "executionArn");
        std::thread::sleep(PAST_THE_EXECUTION_DELAY);
        observed
            .ok("DescribeExecution", &json!({ "executionArn": execution }))
            .await;

        if expected.is_empty() {
            assert!(
                !observed.holds_log_group(&group),
                "case {case:?}: a configuration that logs nothing creates no group either"
            );
            continue;
        }

        // Read back through CloudWatch Logs' OWN reader, which is what a client polls.
        let found = observed
            .logs(
                "FilterLogEvents",
                &json!({ "logGroupName": group.clone() }),
            )
            .await;
        let events = found
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut types: Vec<String> = events
            .iter()
            .filter_map(|event| event.get("message").and_then(Value::as_str))
            .map(|message| {
                let parsed: Value = serde_json::from_str(message)
                    .unwrap_or_else(|_| panic!("case {case:?}: a delivered event is a JSON object, got {message:?}"));
                parsed
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        types.sort();
        let mut wanted: Vec<String> = expected.iter().map(|kind| (*kind).to_string()).collect();
        wanted.sort();
        assert_eq!(
            types, wanted,
            "case {case:?}: expected the event types {wanted:?} in the group, got {events:?}"
        );
        // Every delivered event names the execution it belongs to, and the failure
        // carries the execution data the configuration asked for.
        for event in &events {
            let message = event
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                message.contains(&execution),
                "case {case:?}: expected the delivered event to name its execution, got \
                 {message:?}"
            );
        }
    }
}

/// `includeExecutionData` decides whether the PAYLOAD an event carries is delivered
/// with it: the same run, logged at `ALL`, carries the document it was started with only
/// where the configuration asks for it — while the failure's own error and cause, which
/// are the transition rather than the payload, are delivered either way.
#[tokio::test]
async fn execution_data_is_delivered_only_where_it_is_asked_for() {
    let observed = Observed::new();
    let marker = "payload-marker";
    for (index, (case, include)) in [
        ("the configuration includes execution data", true),
        ("the configuration excludes it", false),
    ]
    .into_iter()
    .enumerate()
    {
        let group = format!("/aws/vendedlogs/states/data-{index}");
        let machine = create(
            &observed,
            &format!("data-{index}"),
            FAILING_DEFINITION,
            "STANDARD",
            Some(logging(observability::LOG_LEVEL_ALL, &group, include)),
        )
        .await;
        let started = observed
            .ok(
                "StartExecution",
                &json!({
                    "stateMachineArn": machine,
                    "name": "run",
                    "input": format!("{{\"marker\":\"{marker}\"}}"),
                }),
            )
            .await;
        let execution = string(&started, "executionArn");
        std::thread::sleep(PAST_THE_EXECUTION_DELAY);
        observed
            .ok("DescribeExecution", &json!({ "executionArn": execution }))
            .await;

        let found = observed
            .logs("FilterLogEvents", &json!({ "logGroupName": group }))
            .await;
        let messages: Vec<String> = found
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|event| event.get("message").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        assert!(
            !messages.is_empty(),
            "case {case:?}: expected the run's events to be delivered, got {found:?}"
        );
        let carries_payload = messages.iter().any(|message| message.contains(marker));
        assert_eq!(
            carries_payload, include,
            "case {case:?}: expected the execution data to be present={include}, got \
             {messages:?}"
        );
        // The error and the cause of the failure are not execution data, so they are
        // delivered whatever the configuration says about the payload.
        assert!(
            messages.iter().any(|message| message.contains("Boom")),
            "case {case:?}: expected the failure's error to be delivered, got {messages:?}"
        );
    }
}

/// A delivery is made into a log group the account ALREADY holds, leaving what is
/// there: the group's retention and the events already in it survive the delivery.
#[tokio::test]
async fn a_delivery_adopts_a_group_the_account_already_holds() {
    let observed = Observed::new();
    let group = "/aws/vendedlogs/states/adopted";
    observed
        .logs("CreateLogGroup", &json!({ "logGroupName": group }))
        .await;
    observed
        .logs(
            "PutRetentionPolicy",
            &json!({ "logGroupName": group, "retentionInDays": 7 }),
        )
        .await;

    let machine = create(
        &observed,
        "adopted",
        HELLO_DEFINITION,
        "STANDARD",
        Some(logging(observability::LOG_LEVEL_ALL, group, true)),
    )
    .await;
    let started = observed
        .ok(
            "StartExecution",
            &json!({ "stateMachineArn": machine, "name": "run" }),
        )
        .await;
    let execution = string(&started, "executionArn");
    std::thread::sleep(PAST_THE_EXECUTION_DELAY);
    observed
        .ok("DescribeExecution", &json!({ "executionArn": execution }))
        .await;

    let described = observed
        .logs(
            "DescribeLogGroups",
            &json!({ "logGroupNamePrefix": group }),
        )
        .await;
    let groups = described
        .get("logGroups")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        groups.len(),
        1,
        "expected the one group the account holds, got {described:?}"
    );
    assert_eq!(
        groups[0].get("retentionInDays").and_then(Value::as_i64),
        Some(7),
        "the delivery leaves the group's own settings alone, got {described:?}"
    );
    let found = observed
        .logs("FilterLogEvents", &json!({ "logGroupName": group }))
        .await;
    let events = found
        .get("events")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    assert!(
        events > 0,
        "expected the run's events in the adopted group, got {found:?}"
    );
}

/// An update whose `loggingConfiguration` is refused leaves the DELIVERY exactly where
/// it was: the state machine keeps the configuration it had, so the executions after
/// the refused update are delivered to the same group as the ones before it.
#[tokio::test]
async fn a_refused_logging_update_leaves_the_delivery_unchanged() {
    let observed = Observed::new();
    let group = "/aws/vendedlogs/states/unchanged";
    let machine = create(
        &observed,
        "unchanged",
        HELLO_DEFINITION,
        "STANDARD",
        Some(logging(observability::LOG_LEVEL_ALL, group, true)),
    )
    .await;

    // The update names a log group ARN without the qualifier the member documents, so
    // it is refused and nothing of the configuration is applied.
    let (status, answer) = observed
        .call(
            "UpdateStateMachine",
            &json!({
                "stateMachineArn": machine,
                "loggingConfiguration": {
                    "level": observability::LOG_LEVEL_ALL,
                    "destinations": [{ "cloudWatchLogsLogGroup": {
                        "logGroupArn": format!(
                            "arn:aws:logs:{REGION}:{ACCOUNT}:log-group:/aws/vendedlogs/states/elsewhere"
                        )
                    } }],
                },
            }),
        )
        .await;
    assert_eq!(
        (status, error_code(&answer).as_str()),
        (400, "InvalidLoggingConfiguration"),
        "expected the unqualified log group ARN to be refused, got {status} with {answer:?}"
    );

    let started = observed
        .ok(
            "StartExecution",
            &json!({ "stateMachineArn": machine, "name": "after" }),
        )
        .await;
    let execution = string(&started, "executionArn");
    std::thread::sleep(PAST_THE_EXECUTION_DELAY);
    observed
        .ok("DescribeExecution", &json!({ "executionArn": execution }))
        .await;

    let found = observed
        .logs("FilterLogEvents", &json!({ "logGroupName": group }))
        .await;
    let events = found
        .get("events")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    assert!(
        events > 0,
        "expected the delivery to the configured group to be unchanged, got {found:?}"
    );
    assert!(
        !observed.holds_log_group("/aws/vendedlogs/states/elsewhere"),
        "expected the refused destination to have taken no delivery"
    );
}

/// A refused start publishes nothing: an execution that never existed is in no series.
#[tokio::test]
async fn a_refused_start_publishes_nothing() {
    let observed = Observed::new();
    let machine = create(&observed, "refused", HELLO_DEFINITION, "STANDARD", None).await;
    let (status, answer) = observed
        .call(
            "StartExecution",
            &json!({ "stateMachineArn": machine, "name": "a name!that is invalid" }),
        )
        .await;
    assert_eq!(
        (status, error_code(&answer).as_str()),
        (400, "InvalidName"),
        "expected the start to be refused, got {status} with {answer:?}"
    );
    let started = observed.series(observability::EXECUTIONS_STARTED_METRIC, &machine);
    assert!(
        started.is_empty(),
        "expected no series for an execution that was never started, got {started:?}"
    );
    // And the clock moving changes nothing about it.
    std::thread::sleep(Duration::from_millis(50));
    let started = observed.series(observability::EXECUTIONS_STARTED_METRIC, &machine);
    assert!(
        started.is_empty(),
        "expected no series to appear later either, got {started:?}"
    );
}
