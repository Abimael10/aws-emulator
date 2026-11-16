// SPDX-License-Identifier: CC0-1.0

//! Where a client READS what a finished canary run came to: Amazon CloudWatch, mounted
//! beside Synthetics on ONE engine and asked through its own `GetMetricStatistics` and
//! `ListMetrics`.
//!
//! The RULE every row asserts: a canary run that reaches a terminal state publishes its own
//! observations into the `CloudWatchSynthetics` namespace, in the series dimensioned by the
//! name of the canary the run belongs to — `SuccessPercent` of 100 for a run that passed and
//! 0 for one that did not, `Failed` of 0 and 1 the same way, and `Duration` of the span the
//! run's own timeline reports, in milliseconds. So every CloudWatch reader of those series
//! reports what the runs came to, a read naming another canary reports nothing, and a canary
//! that has not run has no series at all.
//!
//! The rows differ along what the rule generalises over: the verdict the run reaches (and so
//! the whole value domain of `SuccessPercent` and `Failed`), the form of the grant the
//! execution role carries and the artifact location it is scoped to, how many runs of one
//! canary the series holds, and whether the run is a run of the canary or of one of its dry
//! run configurations.
//!
//! The namespace, the dimension name, the metric names and the units are written here as the
//! CloudWatch Synthetics canary-metric reference names them rather than taken from this
//! crate's own constants, so a series published under a name no AWS reader would ask for
//! fails these rows. Every transition is waited OUT with the target's poll helper rather than
//! slept for.

use serde_json::{json, Value};

use crate::canary_lifecycle::{poll_until, Answer, Synthetics};
use crate::canary_run_outcomes::{
    canary_body, create_bucket, create_role, ready_canary, statement, ARTIFACT_WRITE_ACTION,
    BUCKET, REGION,
};
use crate::group_membership::ok;

/// The namespace CloudWatch Synthetics publishes a canary's run metrics under.
const NAMESPACE: &str = "CloudWatchSynthetics";

/// The dimension those metrics are reported per: the canary the run belongs to.
const CANARY_NAME_DIMENSION: &str = "CanaryName";

/// How much of a run succeeded, as a percentage: 100 for a run that passed, 0 for one that
/// did not.
const SUCCESS_PERCENT: &str = "SuccessPercent";

/// How many of the runs reported failed.
const FAILED: &str = "Failed";

/// How long a run took, in milliseconds.
const DURATION: &str = "Duration";

/// Every metric a canary run publishes, which is what a listing of the namespace reports for
/// a canary that has run.
const RUN_METRICS: [&str; 3] = [SUCCESS_PERCENT, FAILED, DURATION];

/// The `StandardUnit` each of those metrics is published in, in the order above.
const RUN_METRIC_UNITS: [&str; 3] = ["Percent", "Count", "Milliseconds"];

/// The `Period` every read below asks at: the one-minute granularity these datapoints are
/// published at.
const PERIOD_SECS: i64 = 60;

/// How far back a read's window reaches, in seconds, and how far ahead: wide enough to hold
/// the minute a run published into whenever the suite happens to run.
const WINDOW_BEHIND_SECS: i64 = 900;
const WINDOW_AHEAD_SECS: i64 = 300;

/// How far two measurements of one span may differ and still be the same number of
/// milliseconds: the published `Duration` and the span the run's own timeline reports are
/// the SAME instants, so this is floating-point slack and nothing else.
const MILLISECOND_SLACK: f64 = 1.0;

/// The statistics every read asks for, so one answer carries each of the aggregates a row
/// asserts on.
fn statistics() -> Value {
    json!(["SampleCount", "Sum", "Minimum", "Maximum", "Average"])
}

/// The dimension list that selects one canary's series, as CloudWatch takes it in a request.
fn canary_dimensions(canary: &str) -> Value {
    json!([{ "Name": CANARY_NAME_DIMENSION, "Value": canary }])
}

/// Now, in epoch seconds: the instant the windows below are laid around.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("the clock is past the epoch")
        .as_secs() as i64
}

/// A `GetMetricStatistics` request for one canary's `metric`, over a window covering the
/// minute a run of it published into.
fn statistics_request(metric: &str, canary: &str) -> Value {
    let at = now_secs();
    json!({
        "Namespace": NAMESPACE,
        "MetricName": metric,
        "Dimensions": canary_dimensions(canary),
        "StartTime": at - WINDOW_BEHIND_SECS,
        "EndTime": at + WINDOW_AHEAD_SECS,
        "Period": PERIOD_SECS,
        "Statistics": statistics(),
    })
}

/// The `Datapoints` CloudWatch answers for one canary's `metric` RIGHT NOW, without waiting
/// for any to arrive.
async fn datapoints_now(aws: &Synthetics, case: &str, metric: &str, canary: &str) -> Vec<Value> {
    let answer = aws
        .cloudwatch("GetMetricStatistics", statistics_request(metric, canary))
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case}: GetMetricStatistics for {metric} answered {} ({:?})",
        answer.status, answer.body
    );
    answer
        .at("Datapoints")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `Datapoints` CloudWatch answers for one canary's `metric` once at least one has
/// landed: the read is looked at again until the series exists, so no row depends on how
/// fast the host runs.
async fn datapoints(aws: &Synthetics, case: &str, metric: &str, canary: &str) -> Vec<Value> {
    let answer = poll_until(
        &format!("{case}/{metric}"),
        || aws.cloudwatch("GetMetricStatistics", statistics_request(metric, canary)),
        |answer: &Answer| {
            answer
                .at("Datapoints")
                .as_array()
                .is_some_and(|listed| !listed.is_empty())
        },
    )
    .await;
    answer
        .at("Datapoints")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The sum of `field` over `listed`, and how many observations those datapoints stand for.
fn folded(listed: &[Value], field: &str) -> (f64, f64) {
    listed.iter().fold((0.0, 0.0), |(sum, count), datapoint| {
        (
            sum + datapoint[field].as_f64().unwrap_or_default(),
            count + datapoint["SampleCount"].as_f64().unwrap_or_default(),
        )
    })
}

/// The metrics `ListMetrics` reports for `canary` in the namespace, as a sorted list of
/// `<metric>/<dimension set>` so a series published under another dimension name shows up.
async fn listed_series(aws: &Synthetics, case: &str, canary: &str) -> Vec<String> {
    let answer = aws
        .cloudwatch("ListMetrics", json!({ "Namespace": NAMESPACE }))
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case}: ListMetrics answered {} ({:?})",
        answer.status, answer.body
    );
    let mut series: Vec<String> = answer
        .at("Metrics")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|metric| {
            let dimensions: Vec<String> = metric["Dimensions"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|dimension| {
                    format!(
                        "{}={}",
                        dimension["Name"].as_str().unwrap_or_default(),
                        dimension["Value"].as_str().unwrap_or_default()
                    )
                })
                .collect();
            format!(
                "{}/{}",
                metric["MetricName"].as_str().unwrap_or_default(),
                dimensions.join(",")
            )
        })
        .filter(|series| series.contains(canary))
        .collect();
    series.sort();
    series
}

/// Start the canary and wait until its newest run of its OWN schedule is no longer
/// `RUNNING`, then answer that run.
async fn finished_run(aws: &Synthetics, case: &str, name: &str) -> Value {
    ok(&format!("{case}/start"), aws.start(name).await);
    let answer = poll_until(
        &format!("{case}/runs"),
        || aws.runs(name, json!({ "RunType": "CANARY_RUN" })),
        |answer: &Answer| {
            answer.at("CanaryRuns")[0]["Status"]["State"]
                .as_str()
                .is_some_and(|state| state != "RUNNING")
        },
    )
    .await;
    answer.at("CanaryRuns")[0].clone()
}

/// The span `run`'s own timeline reports, in milliseconds: the published `Duration` is this
/// number, so a row asserts the series against the record rather than against a constant.
fn timeline_millis(case: &str, run: &Value) -> f64 {
    let started = run["Timeline"]["Started"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case}: expected a Started instant, got {run}"));
    let completed = run["Timeline"]["Completed"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case}: expected a Completed instant, got {run}"));
    (completed - started) * 1_000.0
}

/// One row of the table below: how the run is set up, and the verdict it therefore reaches.
struct Row {
    case: &'static str,
    /// The canary's name, which is also the key prefix its artifacts go under and the value
    /// of the dimension its series is read by.
    name: &'static str,
    /// The artifact location the canary writes to, relative to the suite's bucket.
    location: String,
    /// The inline permissions policy the execution role carries.
    statements: Value,
    /// Whether the run therefore passes.
    passes: bool,
}

/// RULE: a canary run that finished publishes `SuccessPercent` (100 when it passed, 0 when it
/// did not), `Failed` (0 and 1 the same way) and `Duration` (its own timeline's span in
/// milliseconds) into the `CloudWatchSynthetics` namespace, in the series dimensioned by the
/// canary's name — and in no other canary's series.
#[tokio::test]
async fn a_finished_run_publishes_its_verdict_and_its_duration_for_the_canary() {
    let (aws, iam, s3) = Synthetics::with_metric_reader(REGION);
    create_bucket(&s3).await;

    let rows = [
        Row {
            case: "passed: every action allowed, bucket only",
            name: "metrics-pass-bare",
            location: format!("s3://{BUCKET}"),
            statements: json!([statement("Allow", json!("*"), json!("*"))]),
            passes: true,
        },
        Row {
            case: "passed: the write allowed under the location's own prefix",
            name: "metrics-pass-prefix",
            location: format!("s3://{BUCKET}/team/canaries"),
            statements: json!([statement(
                "Allow",
                json!(ARTIFACT_WRITE_ACTION),
                json!(format!("arn:aws:s3:::{BUCKET}/team/canaries/*")),
            )]),
            passes: true,
        },
        Row {
            case: "failed: a role carrying no permissions at all",
            name: "metrics-fail-no-grant",
            location: format!("s3://{BUCKET}/metrics-fail-no-grant"),
            statements: json!([]),
            passes: false,
        },
        Row {
            case: "failed: the write denied outright",
            name: "metrics-fail-denied",
            location: format!("s3://{BUCKET}/metrics-fail-denied"),
            statements: json!([
                statement("Allow", json!("*"), json!("*")),
                statement(
                    "Deny",
                    json!(ARTIFACT_WRITE_ACTION),
                    json!(format!("arn:aws:s3:::{BUCKET}/*")),
                ),
            ]),
            passes: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let role = create_role(&iam, &format!("{}-role", row.name), row.statements).await;
        ready_canary(
            &aws,
            case,
            canary_body(row.name, &role, &row.location, "rate(5 minutes)"),
        )
        .await;
        let run = finished_run(&aws, case, row.name).await;
        let state = if row.passes { "PASSED" } else { "FAILED" };
        assert_eq!(
            run["Status"]["State"].as_str(),
            Some(state),
            "case {case}: expected the run to reach {state}, got {:?}",
            run["Status"]
        );
        // The run's own record says when its metrics are generated, so the series and the
        // record cannot disagree about that instant.
        assert!(
            run["Timeline"]["MetricTimestampForRunAndRetries"].is_number(),
            "case {case}: expected a finished run to report the instant its metrics are \
             generated at, got {:?}",
            run["Timeline"]
        );

        let expected_percent = if row.passes { 100.0 } else { 0.0 };
        let success = datapoints(&aws, case, SUCCESS_PERCENT, row.name).await;
        let (sum, samples) = folded(&success, "Sum");
        assert_eq!(
            (sum, samples),
            (expected_percent, 1.0),
            "case {case}: expected one observation of {expected_percent} percent, got \
             {success:?}"
        );
        for datapoint in &success {
            assert_eq!(
                datapoint["Average"].as_f64(),
                Some(expected_percent),
                "case {case}: expected an Average of {expected_percent}, got {datapoint:?}"
            );
            assert_eq!(
                datapoint["Unit"].as_str(),
                Some(RUN_METRIC_UNITS[0]),
                "case {case}: expected {} , got {datapoint:?}",
                RUN_METRIC_UNITS[0]
            );
        }

        let expected_failures = if row.passes { 0.0 } else { 1.0 };
        let failed = datapoints(&aws, case, FAILED, row.name).await;
        let (sum, samples) = folded(&failed, "Sum");
        assert_eq!(
            (sum, samples),
            (expected_failures, 1.0),
            "case {case}: expected one observation summing to {expected_failures} failures, \
             got {failed:?}"
        );
        for datapoint in &failed {
            assert_eq!(
                datapoint["Unit"].as_str(),
                Some(RUN_METRIC_UNITS[1]),
                "case {case}: expected {}, got {datapoint:?}",
                RUN_METRIC_UNITS[1]
            );
        }

        let expected_millis = timeline_millis(case, &run);
        let duration = datapoints(&aws, case, DURATION, row.name).await;
        let (sum, samples) = folded(&duration, "Sum");
        assert!(
            (sum - expected_millis).abs() < MILLISECOND_SLACK,
            "case {case}: expected the run's own span of {expected_millis} ms, got {sum} \
             ({duration:?})"
        );
        assert_eq!(
            samples, 1.0,
            "case {case}: expected one observation of the run's duration, got {duration:?}"
        );
        for datapoint in &duration {
            assert_eq!(
                datapoint["Unit"].as_str(),
                Some(RUN_METRIC_UNITS[2]),
                "case {case}: expected {}, got {datapoint:?}",
                RUN_METRIC_UNITS[2]
            );
        }

        // The dimension IS the series identity: the same read against another canary's name
        // reports nothing, whether or not that canary exists.
        for other in ["metrics-nobody", "metrics-pass-bare-x"] {
            let foreign = datapoints_now(&aws, case, SUCCESS_PERCENT, other).await;
            assert!(
                foreign.is_empty(),
                "case {case}: expected nothing in the series of {other}, got {foreign:?}"
            );
        }

        // What a client discovers about the canary: exactly the three series, each
        // dimensioned by the canary's name.
        let listed = listed_series(&aws, case, row.name).await;
        let mut expected: Vec<String> = RUN_METRICS
            .iter()
            .map(|metric| format!("{metric}/{CANARY_NAME_DIMENSION}={}", row.name))
            .collect();
        expected.sort();
        assert_eq!(
            listed, expected,
            "case {case}: expected the canary's three series, got {listed:?}"
        );
    }
}

/// RULE: every run of a canary publishes into the SAME series, so the series holds one
/// observation per run that finished rather than only the newest run's.
///
/// The rows differ along how many runs of one canary the series has to hold.
#[tokio::test]
async fn every_run_of_a_canary_adds_its_own_observation() {
    let (aws, iam, s3) = Synthetics::with_metric_reader(REGION);
    create_bucket(&s3).await;
    let role = create_role(
        &iam,
        "metrics-repeat-role",
        json!([statement("Allow", json!("*"), json!("*"))]),
    )
    .await;

    // A run-once schedule is what lets a canary be started again: it settles to STOPPED once
    // its single run has finished ("Specifying `rate(0 minute)` … causes the canary to run
    // only once when it is started", `CanaryScheduleInput$Expression`).
    let name = "metrics-repeat";
    ready_canary(
        &aws,
        "repeat",
        canary_body(
            name,
            &role,
            &format!("s3://{BUCKET}/{name}"),
            "rate(0 minute)",
        ),
    )
    .await;

    for runs in 1..=3_usize {
        let case = format!("{runs} run(s)");
        let run = finished_run(&aws, &case, name).await;
        assert_eq!(
            run["Status"]["State"].as_str(),
            Some("PASSED"),
            "case {case}: expected the run to pass, got {:?}",
            run["Status"]
        );
        // The canary has to settle back to STOPPED before it can be started again, which is
        // what makes the next run of the loop a SECOND run of the same canary.
        poll_until(
            &format!("{case}/stopped"),
            || aws.get(name),
            |answer: &Answer| answer.string_at("Canary.Status.State") == "STOPPED",
        )
        .await;

        let success = datapoints(&aws, &case, SUCCESS_PERCENT, name).await;
        let (_, samples) = folded(&success, "Sum");
        assert_eq!(
            samples, runs as f64,
            "case {case}: expected one observation per finished run, got {success:?}"
        );
        for datapoint in &success {
            assert_eq!(
                datapoint["Average"].as_f64(),
                Some(100.0),
                "case {case}: expected every passing run to read 100, got {datapoint:?}"
            );
        }
        let failed = datapoints(&aws, &case, FAILED, name).await;
        let (sum, samples) = folded(&failed, "Sum");
        assert_eq!(
            (sum, samples),
            (0.0, runs as f64),
            "case {case}: expected one zero-failure observation per run, got {failed:?}"
        );
    }
}

/// RULE: only a run of the CANARY publishes the canary's series. A canary that has not run
/// has no series at all, and a run of one of its DRY RUN configurations — a configuration
/// the canary does not carry, reported on the dry run itself — adds nothing to it.
#[tokio::test]
async fn a_canary_with_no_run_of_its_own_has_no_series() {
    let (aws, iam, s3) = Synthetics::with_metric_reader(REGION);
    create_bucket(&s3).await;
    let role = create_role(
        &iam,
        "metrics-quiet-role",
        json!([statement("Allow", json!("*"), json!("*"))]),
    )
    .await;

    // A canary that was created and never started, and one whose only run is a dry run.
    for (case, name) in [("never started", "metrics-quiet"), ("dry run only", "metrics-dry")] {
        ready_canary(
            &aws,
            case,
            canary_body(name, &role, &format!("s3://{BUCKET}/{name}"), "rate(5 minutes)"),
        )
        .await;
    }

    let dry = ok(
        "dry run only/start",
        aws.call(
            "POST",
            "/canary/metrics-dry/dry-run/start",
            Some(json!({})),
        )
        .await,
    );
    let dry_run_id = dry.string_at("DryRunConfig.DryRunId");
    // The dry run's own run has to have FINISHED before "it published nothing" means
    // anything: the wait is for that run's verdict, not for a span of time.
    let dry_runs = poll_until(
        "dry run only/runs",
        || {
            aws.runs(
                "metrics-dry",
                json!({ "RunType": "DRY_RUN", "DryRunId": dry_run_id.clone() }),
            )
        },
        |answer: &Answer| {
            answer.at("CanaryRuns")[0]["Status"]["State"]
                .as_str()
                .is_some_and(|state| state != "RUNNING")
        },
    )
    .await;
    let dry_run = dry_runs.at("CanaryRuns")[0].clone();
    assert_eq!(
        dry_run["Status"]["State"].as_str(),
        Some("PASSED"),
        "case dry run only: expected the dry run's run to finish, got {dry_run}"
    );
    assert!(
        dry_run["Timeline"]["MetricTimestampForRunAndRetries"].is_null(),
        "case dry run only: a dry run generates no metrics of the canary, so it reports no \
         instant for them, got {:?}",
        dry_run["Timeline"]
    );

    for (case, name) in [("never started", "metrics-quiet"), ("dry run only", "metrics-dry")] {
        for metric in RUN_METRICS {
            let listed = datapoints_now(&aws, case, metric, name).await;
            assert!(
                listed.is_empty(),
                "case {case}: expected no {metric} series for {name}, got {listed:?}"
            );
        }
        let series = listed_series(&aws, case, name).await;
        assert!(
            series.is_empty(),
            "case {case}: expected {name} to be listed in no series, got {series:?}"
        );
    }
}
