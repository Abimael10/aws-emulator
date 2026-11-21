// SPDX-License-Identifier: CC0-1.0

//! WHICH function a Step Functions `Task` state's work is accounted to, and how a client that
//! polls for those series reads them: `GetMetricStatistics` over CloudWatch's own wire.
//!
//! RULE: a `Task` state whose `Resource` is the ARN of a Lambda function accounts each
//! invocation it takes in the `AWS/States` series dimensioned on EXACTLY that one function's
//! ARN — `LambdaFunctionsScheduled` and `LambdaFunctionsStarted` once per invocation,
//! `LambdaFunctionsSucceeded` or `LambdaFunctionsFailed` once per invocation according to how
//! it closed, one `LambdaFunctionRunTime` sample per invocation — while the executions that
//! took them are accounted per state machine and Lambda accounts the same invocations in
//! `AWS/Lambda` per function name. The dimension value is the function the `Resource` NAMED,
//! so work taken on another function is counted in another series and in neither the other's;
//! a series nothing was accounted in answers with no datapoint; and because `Unit` only
//! SELECTS among the units an observation was published with, every one of those series reads
//! the same whether the read names the series' unit or names none at all.
//!
//! That last part is what a POLLING client depends on: it asks for a sum, over a window, for a
//! dimension set it built from the ARNs it holds, and a barrier it waits on can only settle if
//! the series it asks for is the series the work was published to. A family published under no
//! dimension, under the state machine's, or only under a unit the read does not name, leaves
//! such a client waiting instead of answering it.
//!
//! Step Functions decides and makes the invocation and accounts both families, IAM holds the
//! document the invocation is judged against, Lambda runs the handler and accounts its own
//! series, and CloudWatch answers for every series, so the suite lives here rather than in any
//! one of those crates. The deployment, the role, the state machine and the execution wait are
//! the sibling suite's ([`crate::sfn_lambda_task_metric_attribution`]), which pins the
//! magnitudes one function's series carry; this suite pins WHICH series carry them.
//!
//! The rows differ along what the rule generalises over: which function a state machine's task
//! names, and the mix of outcomes that function's invocations close with (one returning and one
//! raising; one returning alone; two raising) — so each counter the rule names carries a
//! non-zero magnitude in some row and is the absent series in another, no row's expected sums
//! are a magnitude another row's work could produce, and every read is taken twice, once naming
//! the series' unit and once naming none.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// `tests/all/` is ONE crate root, so the deployment of an inline handler, the role it is judged
// under, the state machine that invokes it, the wait for an execution to leave `RUNNING`, the
// read of one series and the names every series and dimension is asked for are borrowed from
// the sibling suite rather than copied. That suite's `datapoints` takes the unit to name as an
// `Option`, so the read this suite needs — the one naming NO unit — is served by the one reader
// both suites use.
use crate::sfn_lambda_task_metric_attribution::{
    build_router, datapoints, function_arn_dimensions, function_name_dimensions, given_function,
    given_machine, given_role, ran, state_machine_dimensions, string, sum_of, BOOM, COUNT_UNIT,
    ERRORS, EXECUTIONS_FAILED, EXECUTIONS_SUCCEEDED, FAILED, FUNCTION_NAME_DIMENSION, INVOCATIONS,
    INVOKE_ACTION, LAMBDA_FUNCTIONS_FAILED, LAMBDA_FUNCTIONS_SCHEDULED, LAMBDA_FUNCTIONS_STARTED,
    LAMBDA_FUNCTIONS_SUCCEEDED, LAMBDA_FUNCTION_ARN_DIMENSION, LAMBDA_FUNCTION_RUN_TIME,
    LAMBDA_NAMESPACE, LAMBDA_PRINCIPAL, LOG_ACTION, MILLISECONDS_UNIT, OK, RAISED_CLASS,
    SAMPLE_COUNT_STATISTIC, STATES_NAMESPACE, STATES_PRINCIPAL, STATE_MACHINE_ARN_DIMENSION,
    SUCCEEDED, SUM_STATISTIC,
};

/// One `statistic` of a series over the window, read BOTH ways: naming the unit the series is
/// published with, and naming none. The two cannot disagree, because naming no unit returns
/// the data collected with any unit; the magnitude they agree on is answered.
async fn read_either_way(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    metric: &str,
    statistic: &str,
    unit: &str,
    dimensions: &Value,
) -> f64 {
    let with_unit = sum_of(
        &datapoints(router, case, namespace, metric, Some(unit), dimensions).await,
        statistic,
    );
    let without_unit = sum_of(
        &datapoints(router, case, namespace, metric, None, dimensions).await,
        statistic,
    );
    assert_eq!(
        with_unit, without_unit,
        "case {case:?}: {namespace} {metric} {statistic} under {dimensions} must read the same \
         whether the read names {unit} or no unit, got {with_unit} naming it and {without_unit} \
         naming none"
    );
    without_unit
}

/// The `Sum` of a counter's series over the window — the magnitude a client charts, alarms or
/// polls on — read both ways.
async fn counted_either_way(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    metric: &str,
    dimensions: &Value,
) -> f64 {
    read_either_way(
        router,
        case,
        namespace,
        metric,
        SUM_STATISTIC,
        COUNT_UNIT,
        dimensions,
    )
    .await
}

/// How many SAMPLES a span's series holds over the window — one per observation published,
/// which for a run span is one per invocation that closed — read both ways, as above.
async fn sampled_either_way(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    dimensions: &Value,
) -> f64 {
    read_either_way(
        router,
        case,
        STATES_NAMESPACE,
        metric,
        SAMPLE_COUNT_STATISTIC,
        MILLISECONDS_UNIT,
        dimensions,
    )
    .await
}

/// One row: one function, one state machine whose only `Task` names that function's ARN, and
/// the invocations that machine's executions take on it. Every counter the rule names is a
/// function of `runs` alone, so no row states one twice.
struct Row {
    case: &'static str,
    /// Each execution this row's state machine runs, as `(the mode the handler is asked to
    /// take, the value it is asked to echo)`.
    runs: &'static [(&'static str, i64)],
}

/// Three functions, each invoked by its own state machine, with run mixes whose expected sums
/// differ from each other and from their total — so a family published under the wrong
/// function's ARN, or under one series for every function, cannot satisfy every row.
fn rows() -> Vec<Row> {
    vec![
        // The shape the behaviour was recorded in: one execution that succeeds and one that
        // fails, so the closing counters differ from the scheduled and started ones.
        Row {
            case: "a function invoked once returning and once raising",
            runs: &[(OK, 11), (BOOM, 0)],
        },
        Row {
            case: "a second function invoked once, returning",
            runs: &[(OK, 22)],
        },
        // A row whose every invocation raises, so `LambdaFunctionsFailed` carries a non-zero
        // magnitude somewhere in the table and `LambdaFunctionsSucceeded` is the absent one.
        Row {
            case: "a third function invoked twice, raising both times",
            runs: &[(BOOM, 0), (BOOM, 33)],
        },
    ]
}

/// What one row's work is, once it has run: the resources it took it on and the magnitudes its
/// series must carry.
struct Accounted {
    case: &'static str,
    function_name: String,
    function_arn: String,
    machine_arn: String,
    invocations: f64,
    succeeded: f64,
    failed: f64,
}

/// Deploy one row's function and state machine, run its executions, and report what the row
/// accounted. Each outcome is checked against the run that produced it, so a counter is only
/// ever compared against work that really happened.
async fn run_row(router: &MultiServiceRouter, index: usize, row: &Row) -> Accounted {
    let Row { case, runs } = row;
    let function_name = format!("xsvc-named-arn-{index}");
    let function_role = given_role(
        router,
        &format!("xsvc-named-arn-function-role-{index}"),
        LAMBDA_PRINCIPAL,
        LOG_ACTION,
        "*",
    )
    .await;
    let function_arn = given_function(
        router,
        case,
        &format!("xsvc-named-arn-stack-{index}"),
        &function_name,
        &function_role,
    )
    .await;
    // The execution role carries the grant the invocation is judged against, on this row's own
    // function ARN.
    let machine_role = given_role(
        router,
        &format!("xsvc-named-arn-states-role-{index}"),
        STATES_PRINCIPAL,
        INVOKE_ACTION,
        &function_arn,
    )
    .await;
    let machine_arn = given_machine(
        router,
        case,
        &format!("xsvc-named-arn-{index}"),
        &function_arn,
        &machine_role,
    )
    .await;

    let mut succeeded = 0.0;
    let mut failed = 0.0;
    for (run, (mode, echo)) in runs.iter().enumerate() {
        let input = json!({ "mode": mode, "echo": echo });
        let execution = ran(router, case, &machine_arn, &format!("run-{run}"), &input).await;
        let status = execution.get("status").and_then(Value::as_str);
        if *mode == OK {
            succeeded += 1.0;
            assert_eq!(
                status,
                Some(SUCCEEDED),
                "case {case:?} run {run}: expected {SUCCEEDED}, got {execution}"
            );
            let reported = string(&execution, "output");
            let output: Value = serde_json::from_str(&reported).unwrap_or_else(|error| {
                panic!("case {case:?} run {run}: expected a JSON output, got {reported:?} ({error})")
            });
            assert_eq!(
                output,
                json!({ "echo": echo }),
                "case {case:?} run {run}: expected the handler's own return value, got {output}"
            );
        } else {
            failed += 1.0;
            assert_eq!(
                status,
                Some(FAILED),
                "case {case:?} run {run}: expected {FAILED}, got {execution}"
            );
            assert_eq!(
                execution.get("error").and_then(Value::as_str),
                Some(RAISED_CLASS),
                "case {case:?} run {run}: expected the raised class as the error, got {execution}"
            );
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let invocations = runs.len() as f64;
    Accounted {
        case,
        function_name,
        function_arn,
        machine_arn,
        invocations,
        succeeded,
        failed,
    }
}

/// Each row's invocations are accounted under the ARN of the function its `Task` NAMED, each
/// row's executions under its own state machine, and Lambda's own series under that function's
/// name — every series reading the same whether the read names its unit or names none, and no
/// row's work appearing in another row's or another family's series.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lambda_tasks_work_is_accounted_under_the_function_arn_it_named() {
    let router = build_router();

    let mut accounted = Vec::new();
    for (index, row) in rows().iter().enumerate() {
        accounted.push(run_row(&router, index, row).await);
    }

    for row in &accounted {
        let case = row.case;
        let per_function = function_arn_dimensions(&row.function_arn);
        for (metric, expected) in [
            (LAMBDA_FUNCTIONS_SCHEDULED, row.invocations),
            (LAMBDA_FUNCTIONS_STARTED, row.invocations),
            (LAMBDA_FUNCTIONS_SUCCEEDED, row.succeeded),
            (LAMBDA_FUNCTIONS_FAILED, row.failed),
        ] {
            let actual =
                counted_either_way(&router, case, STATES_NAMESPACE, metric, &per_function).await;
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {STATES_NAMESPACE} {metric} under \
                 {LAMBDA_FUNCTION_ARN_DIMENSION} {arn:?} to sum to {expected}, got {actual}",
                arn = row.function_arn
            );
        }
        let samples =
            sampled_either_way(&router, case, LAMBDA_FUNCTION_RUN_TIME, &per_function).await;
        assert_eq!(
            samples, row.invocations,
            "case {case:?}: expected {invocations} {LAMBDA_FUNCTION_RUN_TIME} samples under \
             {LAMBDA_FUNCTION_ARN_DIMENSION} {arn:?}, got {samples}",
            invocations = row.invocations,
            arn = row.function_arn
        );

        // The executions that took that work are accounted per state machine, and the two
        // families never cross: neither appears under the other's dimension, read with no unit
        // named so a datapoint published under any unit would be seen.
        let per_machine = state_machine_dimensions(&row.machine_arn);
        for (metric, expected) in [
            (EXECUTIONS_SUCCEEDED, row.succeeded),
            (EXECUTIONS_FAILED, row.failed),
        ] {
            let actual =
                counted_either_way(&router, case, STATES_NAMESPACE, metric, &per_machine).await;
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {STATES_NAMESPACE} {metric} under \
                 {STATE_MACHINE_ARN_DIMENSION} to sum to {expected}, got {actual}"
            );
        }
        for metric in [
            LAMBDA_FUNCTIONS_SCHEDULED,
            LAMBDA_FUNCTIONS_STARTED,
            LAMBDA_FUNCTIONS_SUCCEEDED,
            LAMBDA_FUNCTIONS_FAILED,
            LAMBDA_FUNCTION_RUN_TIME,
        ] {
            let held = datapoints(
                &router,
                case,
                STATES_NAMESPACE,
                metric,
                None,
                &per_machine,
            )
            .await;
            assert!(
                held.is_empty(),
                "case {case:?}: {metric} belongs to {LAMBDA_FUNCTION_ARN_DIMENSION} alone, yet \
                 {STATE_MACHINE_ARN_DIMENSION} answered {held:?}"
            );
        }
        for metric in [EXECUTIONS_SUCCEEDED, EXECUTIONS_FAILED] {
            let held = datapoints(
                &router,
                case,
                STATES_NAMESPACE,
                metric,
                None,
                &per_function,
            )
            .await;
            assert!(
                held.is_empty(),
                "case {case:?}: {metric} belongs to {STATE_MACHINE_ARN_DIMENSION} alone, yet \
                 {LAMBDA_FUNCTION_ARN_DIMENSION} answered {held:?}"
            );
        }

        // And Lambda accounted the same invocations in its own namespace, per function name.
        let per_name = function_name_dimensions(&row.function_name);
        let admitted =
            counted_either_way(&router, case, LAMBDA_NAMESPACE, INVOCATIONS, &per_name).await;
        assert!(
            admitted >= row.invocations,
            "case {case:?}: expected at least {invocations} {LAMBDA_NAMESPACE} {INVOCATIONS} \
             under {FUNCTION_NAME_DIMENSION} {name:?}, got {admitted}",
            invocations = row.invocations,
            name = row.function_name
        );
        let errored = counted_either_way(&router, case, LAMBDA_NAMESPACE, ERRORS, &per_name).await;
        assert_eq!(
            errored, row.failed,
            "case {case:?}: expected {LAMBDA_NAMESPACE} {ERRORS} under \
             {FUNCTION_NAME_DIMENSION} {name:?} to sum to {failed}, got {errored}",
            failed = row.failed,
            name = row.function_name
        );
    }

    // Each row's sums above are its OWN invocations rather than every row's, which is what
    // says the dimension value is the ARN the `Task` named; and an ARN no `Task` named is a
    // series nothing was accounted in, which answers with no datapoint rather than refusing or
    // carrying another function's work. The suffix keeps the value ARN-SHAPED — a function ARN
    // of this account and Region whose name no row deployed — so the read exercises the series
    // lookup rather than any check on the dimension's form.
    let untouched = format!("{}-untouched", accounted[0].function_arn);
    for metric in [
        LAMBDA_FUNCTIONS_SCHEDULED,
        LAMBDA_FUNCTIONS_STARTED,
        LAMBDA_FUNCTIONS_SUCCEEDED,
        LAMBDA_FUNCTIONS_FAILED,
        LAMBDA_FUNCTION_RUN_TIME,
    ] {
        let case = "an ARN no task named";
        let held = datapoints(
            &router,
            case,
            STATES_NAMESPACE,
            metric,
            None,
            &function_arn_dimensions(&untouched),
        )
        .await;
        assert!(
            held.is_empty(),
            "case {case:?}: no invocation was taken on {untouched:?}, yet {metric} under \
             {LAMBDA_FUNCTION_ARN_DIMENSION} answered {held:?}"
        );
    }
}
