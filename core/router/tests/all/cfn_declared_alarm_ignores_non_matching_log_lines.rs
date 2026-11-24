// SPDX-License-Identifier: CC0-1.0

//! The NEGATIVE control of the monitoring estate one stack declares — a log group, the metric
//! filter over its lines and the alarm over the metric that filter publishes — read and driven
//! through the three services that own it, on the emulator `core/router/src/lib.rs` assembles for
//! the binary.
//!
//! The RULE under test: **an alarm a stack reports created is readable in CloudWatch from the
//! moment the stack completes — one `MetricAlarms` entry under the name the stack publishes,
//! carrying a state, with its creation already recorded in `DescribeAlarmHistory` — and a log
//! event its stack's own metric filter does NOT match publishes no datapoint into the series that
//! filter feeds, so it moves neither that alarm's state nor its history, while an event the same
//! filter DOES match moves both.** A client can therefore tell "the filter correctly ignored the
//! event" from "no alarm evaluation ever happened": the first is a non-empty listing whose state
//! is the one the alarm's `TreatMissingData` implies and whose history holds exactly the
//! transitions that treatment owed, the second is the empty listing this suite refuses.
//!
//! The reported case deployed exactly that estate, put a NON-matching event and then indexed
//! entry `[0]` of `DescribeAlarms(AlarmNames=[<the published name>])` to read `StateValue` — and
//! found the listing EMPTY, so the negative control could not be run at all: with no state and no
//! history row for the deployed alarm there is nothing to compare against.
//!
//! The suites this one divides the rule with, so the overlap stays visible:
//! `core/router/tests/all/cfn_declared_log_alarm_evaluates_on_ingested_lines.rs` pins the POSITIVE
//! half over the same estate (the declared configuration as CloudWatch reports it, the settling
//! transition, and the move a matching line drives) and
//! `core/router/tests/all/logs_metric_filter_drives_the_alarm_notification_chain.rs` pins the
//! extraction-to-notification chain for a filter and an alarm a CLIENT put. What only this suite
//! reaches is what a non-matching event does to a stack-deployed alarm: nothing, with the alarm
//! still readable.
//!
//! Both of those suites are imported rather than copied: the assembled router, the three services'
//! wire calls, the stack poll, the alarm read, the history read and the transition summary come
//! from the first, and the aggregate arithmetic a row grades its own window with from the second.
//! What is this suite's own is the declaration it deploys (every row names a `TreatMissingData`,
//! which the sibling suite fixes), the non-matching ingest and the metric read that shows a series
//! nothing was published into.
//!
//! The rows differ along what the rule generalises over rather than restating that one template:
//! the FORM of the declared `FilterPattern` (an unstructured term, a JSON pattern over a text
//! member, a JSON pattern over a numeric member, a space-delimited field list) and so the form of
//! the lines that miss it, the `MetricValue` the transformation publishes (a literal and a
//! selected field), the `Statistic` the alarm aggregates a period with, the direction of its
//! `ComparisonOperator`, the `Threshold` spelled as an integer and as a fraction, and every value
//! of `TreatMissingData` — which is what decides the state an alarm over a series nothing has
//! published into is readable in, so no row shares the state another row expects.
//!
//! Nothing is waited out: the stacks are asked for together, each is polled to the status it
//! settles on, and every alarm state is polled until the evaluation its owning service's clock
//! owes has been materialised. While no datapoint exists the alarm's state is a fixed point of
//! that evaluation, which is why the readings the negative control makes are exact rather than
//! prefixes.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cfn_declared_log_alarm_evaluates_on_ingested_lines::{
    cfn, cw, described_alarm, history, logs, now_ms, router, settled_alarm, settled_stack,
    transition, DEADLINE, LOOK_AGAIN, PERIOD_SECS, STATE_ALARM, STATE_INSUFFICIENT_DATA, STATE_OK,
    STATE_UPDATE,
};
use crate::logs_metric_filter_drives_the_alarm_notification_chain::{aggregate, breaches};
use crate::query_wire::{elements, opt_element, outputs_of};

/// The status a stack whose resources were all created reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `HistoryItemType` a configuration write is recorded under
/// (`smithy_query(query="shape",shape="HistoryItemType")`), which is the item the deploy's own
/// `PutMetricAlarm` leaves behind.
const CONFIGURATION_UPDATE: &str = "ConfigurationUpdate";

/// Every value of `TreatMissingData`, as "Sets how this alarm is to handle missing data points.
/// Valid values are breaching, notBreaching, ignore, and missing."
/// (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="TreatMissingData")`) names them —
/// the catalog the rows below span, since this member alone decides the state the deployed alarm is
/// readable in while nothing has been published into the series it watches.
const TREAT_BREACHING: &str = "breaching";
const TREAT_NOT_BREACHING: &str = "notBreaching";
const TREAT_IGNORE: &str = "ignore";
const TREAT_MISSING: &str = "missing";

/// How many of the alarm's periods one evaluation grades. One, so the window a row's own lines land
/// in is the window its alarm is graded on.
const EVALUATION_PERIODS: i64 = 1;

/// The logical ids the template declares its resources under, and the output keys it publishes the
/// alarm's and the group's identity under.
const GROUP_ID: &str = "Group";
const STREAM_ID: &str = "Stream";
const FILTER_ID: &str = "Filter";
const ALARM_ID: &str = "Alarm";
const ALARM_NAME_OUTPUT: &str = "AlarmName";
const GROUP_NAME_OUTPUT: &str = "LogGroupName";

/// Seconds since the epoch, the unit a metric read's range is stated in
/// (`GetMetricStatisticsInput$StartTime`).
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64()
}

/// One row: the estate one template declares, the lines that miss the filter it declares, and the
/// lines that hit it.
struct Row {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The stack, the log group, the log stream, the filter and the alarm this row deploys.
    stack: &'static str,
    group: &'static str,
    stream: &'static str,
    filter: &'static str,
    alarm: &'static str,
    /// The series the filter publishes into and the alarm watches: one per row, so a row reads its
    /// own metric and no other.
    namespace: &'static str,
    metric: &'static str,
    /// The `FilterPattern` the declaration carries and the `MetricValue` its one transformation
    /// publishes.
    pattern: &'static str,
    metric_value: &'static str,
    /// The `Statistic` the alarm aggregates the period with
    /// (`smithy_query(query="shape",shape="Statistic")`).
    statistic: &'static str,
    /// The `ComparisonOperator` it grades that aggregate with
    /// (`smithy_query(query="shape",shape="ComparisonOperator")`).
    comparison: &'static str,
    /// The `Threshold` exactly as the template spells it — an integer or a fraction, both of which
    /// CloudWatch holds as one `Double` (`PutMetricAlarmInput$Threshold`).
    threshold: Value,
    /// How the alarm treats a window nothing was published into, which decides the state it is
    /// readable in while the series is empty.
    treat_missing: &'static str,
    /// Lines the declared pattern does NOT match, so the declared transformation publishes nothing
    /// for them — it declares no `DefaultValue`.
    non_matching: &'static [&'static str],
    /// Lines the declared pattern DOES match, each paired with the value the transformation
    /// publishes for it.
    matching: &'static [(&'static str, f64)],
}

/// The rows, spanning the class the rule covers.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the reported estate: an unstructured term the quiet lines do not carry, a \
                   literal metric value, Sum over an integer threshold, notBreaching",
            stack: "sftp-quiet-term",
            group: "/aws/transfer/sftp-quiet-term",
            stream: "sftp-quiet-term-stream",
            filter: "sftp-quiet-term-filter",
            alarm: "sftp-quiet-term-alarm",
            namespace: "Sftp/Diagnostics",
            metric: "SftpErrors",
            pattern: "ERROR",
            metric_value: "1",
            statistic: "Sum",
            comparison: "GreaterThanOrEqualToThreshold",
            threshold: json!(1),
            treat_missing: TREAT_NOT_BREACHING,
            non_matching: &["INFO session opened", "WARN slow upload", "INFO session closed"],
            matching: &[("ERROR upload refused", 1.0), ("ERROR disk full", 1.0)],
        },
        Row {
            case: "a JSON pattern over a text member, SampleCount over a fractional threshold, \
                   missing",
            stack: "sftp-quiet-json",
            group: "/aws/transfer/sftp-quiet-json",
            stream: "sftp-quiet-json-stream",
            filter: "sftp-quiet-json-filter",
            alarm: "sftp-quiet-json-alarm",
            namespace: "Sftp/Structured",
            metric: "StructuredErrors",
            pattern: r#"{ $.level = "ERROR" }"#,
            metric_value: "1",
            statistic: "SampleCount",
            comparison: "GreaterThanThreshold",
            threshold: json!(0.5),
            treat_missing: TREAT_MISSING,
            non_matching: &[
                r#"{"level":"INFO","session":2}"#,
                r#"{"level":"WARN","session":3}"#,
            ],
            matching: &[(r#"{"level":"ERROR","session":4}"#, 1.0)],
        },
        Row {
            case: "a space-delimited field list the quiet lines have the wrong shape for, a \
                   selected field as the metric value, Maximum under its threshold, breaching",
            stack: "sftp-quiet-fields",
            group: "/aws/transfer/sftp-quiet-fields",
            stream: "sftp-quiet-fields-stream",
            filter: "sftp-quiet-fields-filter",
            alarm: "sftp-quiet-fields-alarm",
            namespace: "Sftp/Transfers",
            metric: "TransferredBytes",
            pattern: "[ip, status, size]",
            metric_value: "$size",
            statistic: "Maximum",
            comparison: "GreaterThanThreshold",
            threshold: json!(1024),
            treat_missing: TREAT_BREACHING,
            non_matching: &["unparseable", "10.0.0.9 500"],
            matching: &[("10.0.0.1 500 512", 512.0), ("10.0.0.2 500 256", 256.0)],
        },
        Row {
            case: "a JSON pattern over a numeric member, that member as the metric value, Minimum \
                   at its threshold, ignore",
            stack: "sftp-quiet-status",
            group: "/aws/transfer/sftp-quiet-status",
            stream: "sftp-quiet-status-stream",
            filter: "sftp-quiet-status-filter",
            alarm: "sftp-quiet-status-alarm",
            namespace: "Sftp/Responses",
            metric: "WorstStatus",
            pattern: "{ $.status = 500 }",
            metric_value: "$.status",
            statistic: "Minimum",
            comparison: "GreaterThanOrEqualToThreshold",
            threshold: json!(500),
            treat_missing: TREAT_IGNORE,
            non_matching: &[r#"{"status":200}"#, r#"{"status":301}"#],
            matching: &[(r#"{"status":500,"request":7}"#, 500.0)],
        },
    ]
}

/// The template a row deploys: the log group, the stream inside it, the metric filter over its
/// lines and the alarm over the metric that filter publishes, with the alarm's name and the group's
/// name as the stack's outputs.
///
/// `Ref` of an `AWS::CloudWatch::Alarm` returns the alarm's name and `Ref` of an
/// `AWS::Logs::LogGroup` returns the group's, which is the handle every call below takes.
fn template_of(row: &Row) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a log group, a metric filter over its lines and an alarm over the \
                        metric that filter publishes",
        "Resources": {
            GROUP_ID: {
                "Type": "AWS::Logs::LogGroup",
                "Properties": {"LogGroupName": row.group},
            },
            STREAM_ID: {
                "Type": "AWS::Logs::LogStream",
                "Properties": {
                    "LogGroupName": {"Ref": GROUP_ID},
                    "LogStreamName": row.stream,
                },
            },
            FILTER_ID: {
                "Type": "AWS::Logs::MetricFilter",
                "Properties": {
                    "LogGroupName": {"Ref": GROUP_ID},
                    "FilterName": row.filter,
                    "FilterPattern": row.pattern,
                    "MetricTransformations": [{
                        "MetricNamespace": row.namespace,
                        "MetricName": row.metric,
                        "MetricValue": row.metric_value,
                    }],
                },
            },
            ALARM_ID: {
                "Type": "AWS::CloudWatch::Alarm",
                "DependsOn": FILTER_ID,
                "Properties": {
                    "AlarmName": row.alarm,
                    "Namespace": row.namespace,
                    "MetricName": row.metric,
                    "Statistic": row.statistic,
                    "Period": PERIOD_SECS,
                    "EvaluationPeriods": EVALUATION_PERIODS,
                    "Threshold": row.threshold,
                    "ComparisonOperator": row.comparison,
                    "TreatMissingData": row.treat_missing,
                },
            },
        },
        "Outputs": {
            ALARM_NAME_OUTPUT: {"Value": {"Ref": ALARM_ID}},
            GROUP_NAME_OUTPUT: {"Value": {"Ref": GROUP_ID}},
        },
    })
    .to_string()
}

/// The state an alarm over a series nothing has been published into settles in, for each value of
/// `TreatMissingData`.
///
/// Source: that member's own documentation
/// (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="TreatMissingData")`) —
/// "breaching — Missing data points are treated as breaching the threshold", "notBreaching —
/// Missing data points are treated as being within the threshold", "ignore — The current alarm
/// state is maintained" and "missing — ... the alarm transitions to INSUFFICIENT_DATA", with the
/// maintained state being the `INSUFFICIENT_DATA` a create sets.
fn settles_in(case: &str, treat_missing: &str) -> &'static str {
    match treat_missing {
        TREAT_BREACHING => STATE_ALARM,
        TREAT_NOT_BREACHING => STATE_OK,
        TREAT_IGNORE | TREAT_MISSING => STATE_INSUFFICIENT_DATA,
        other => panic!("case {case:?}: no row states the missing-data treatment {other:?}"),
    }
}

/// The `StateUpdate` summaries an alarm that settled in `settled` has recorded: the transition out
/// of the state its creation set, or none at all for a treatment that leaves it in that state.
fn settling_history(settled: &str) -> Vec<String> {
    if settled == STATE_INSUFFICIENT_DATA {
        Vec::new()
    } else {
        vec![transition(STATE_INSUFFICIENT_DATA, settled)]
    }
}

/// Ingest `lines` into the stream the row's own stack declared inside the row's own group, as one
/// batch stamped one period back — so they land in the window the next evaluation grades rather
/// than in the window the clock is still in.
async fn ingest(router: &MultiServiceRouter, row: &Row, lines: &[&str]) {
    let timestamp = now_ms() - PERIOD_SECS * 1_000;
    let events: Vec<Value> = lines
        .iter()
        .map(|message| json!({"timestamp": timestamp, "message": message}))
        .collect();
    let accepted = logs(
        router,
        row.case,
        "PutLogEvents",
        &json!({
            "logGroupName": row.group,
            "logStreamName": row.stream,
            "logEvents": events,
        }),
    )
    .await;
    assert!(
        accepted.get("rejectedLogEventsInfo").is_none(),
        "case {:?}: the batch must be accepted whole, got {accepted}",
        row.case
    );
}

/// Every datapoint CloudWatch reports for the row's series over a span wide enough to hold every
/// window this suite ingests into.
async fn datapoints(router: &MultiServiceRouter, row: &Row) -> Vec<Value> {
    #[allow(clippy::cast_precision_loss)]
    let span = 5.0 * PERIOD_SECS as f64;
    let at = now_secs();
    let answered = cw(
        router,
        row.case,
        "GetMetricStatistics",
        &json!({
            "Namespace": row.namespace,
            "MetricName": row.metric,
            "StartTime": at - span,
            "EndTime": at + span,
            "Period": PERIOD_SECS,
            "Statistics": ["SampleCount", "Sum", "Minimum", "Maximum"],
        }),
    )
    .await;
    answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Ingest the row's MATCHING lines until its alarm reports `wanted`.
///
/// Each round ingests one batch and then reads, so the window a batch lands in is the window the
/// read's own evaluation grades however the minute boundary fell between them — the wait is on the
/// state, never on a span of this host's clock. Repeating the batch cannot change the verdict a row
/// expects: every row grades its window with a statistic that is invariant under the same batch
/// arriving again (its extremes) or moves further in the direction its comparison already crossed
/// (its total and its count).
async fn moved_to(router: &MultiServiceRouter, row: &Row, wanted: &str) {
    let lines: Vec<&str> = row.matching.iter().map(|(line, _)| *line).collect();
    let deadline = Instant::now() + DEADLINE;
    loop {
        ingest(router, row, &lines).await;
        let reported = described_alarm(router, row.case, row.alarm).await;
        if reported["StateValue"].as_str() == Some(wanted) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: the alarm {:?} never reported {wanted:?} over its own filter's matching \
             lines, last read {reported}",
            row.case,
            row.alarm
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: see the module header — the alarm a stack deploys is readable with a state and a history
/// from the moment the stack completes, and a log line its own metric filter does not match
/// publishes no datapoint and moves neither of them, while one it does match moves both.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_non_matching_event_moves_nothing_of_a_stack_deployed_metric_filter_alarm() {
    let router = router();
    let rows = rows();

    // Every row's stack is asked for first, so the settling of one overlaps the settling of the
    // others rather than being waited out in turn.
    for row in &rows {
        cfn(
            &router,
            row.case,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &template_of(row)),
            ],
        )
        .await;
    }

    for row in &rows {
        let case = row.case;

        // 1. The stack completes and publishes the identity of the alarm and of the group.
        let described = settled_stack(&router, case, row.stack).await;
        let status = opt_element(&described, "StackStatus").unwrap_or_default();
        if status != CREATE_COMPLETE {
            let events = cfn(
                &router,
                case,
                "DescribeStackEvents",
                &[("StackName", row.stack)],
            )
            .await;
            let reasons = elements(&events, "ResourceStatusReason");
            panic!(
                "case {case:?}: the stack must reach {CREATE_COMPLETE}, got {status:?} with \
                 resource failures {reasons:?}"
            );
        }
        let published = outputs_of(&described);
        assert_eq!(
            published.get(ALARM_NAME_OUTPUT).map(String::as_str),
            Some(row.alarm),
            "case {case:?}: expected the stack to publish the declared alarm name, got \
             {published:?}"
        );
        assert_eq!(
            published.get(GROUP_NAME_OUTPUT).map(String::as_str),
            Some(row.group),
            "case {case:?}: expected the stack to publish the declared log group name, got \
             {published:?}"
        );

        // 2. The reading the negative control opens with, made the moment the stack completed:
        //    entry [0] of the listing for the published name is readable, and the deploy's own
        //    configuration write is already a history row — so a client has something to compare
        //    against before it puts a single event. The state that entry carries is asserted by
        //    step 3, which knows the one value this row's treatment owes it.
        described_alarm(&router, case, row.alarm).await;
        let configured = history(&router, case, row.alarm, CONFIGURATION_UPDATE).await;
        assert_eq!(
            configured,
            vec![format!("Alarm \"{}\" created", row.alarm)],
            "case {case:?}: expected the deploy's own configuration write to be the alarm's one \
             recorded {CONFIGURATION_UPDATE}, got {configured:?}"
        );

        // 3. Its own evaluation settles it in the state its missing-data treatment implies for a
        //    series nothing has published into, recording that transition and no other.
        let settled = settles_in(case, row.treat_missing);
        settled_alarm(&router, case, row.alarm, settled).await;
        let settling = settling_history(settled);
        let recorded = history(&router, case, row.alarm, STATE_UPDATE).await;
        assert_eq!(
            recorded, settling,
            "case {case:?}: expected the settling of the deployed alarm under \
             {:?} to be exactly its recorded transitions, got {recorded:?}",
            row.treat_missing
        );

        // 4. The negative control: lines the declared pattern does not match are ingested into the
        //    stack's own stream, in the window the next evaluation grades.
        ingest(&router, row, row.non_matching).await;

        //    The declared transformation publishes NOTHING for them, so the series the alarm
        //    watches still has no datapoint at all.
        let published_data = datapoints(&router, row).await;
        assert!(
            published_data.is_empty(),
            "case {case:?}: a line the declared pattern does not match must publish no datapoint \
             into {}/{}, got {published_data:?}",
            row.namespace,
            row.metric
        );

        //    And the alarm is untouched: still one readable entry, still the state its treatment
        //    implies, still exactly the transitions its settling recorded. With no datapoint in the
        //    series that state is a fixed point of the evaluation, so these readings cannot depend
        //    on when they were made.
        let quiet = described_alarm(&router, case, row.alarm).await;
        assert_eq!(
            quiet["StateValue"].as_str(),
            Some(settled),
            "case {case:?}: a non-matching line must leave the alarm in {settled:?}, got {quiet}"
        );
        let after_quiet = history(&router, case, row.alarm, STATE_UPDATE).await;
        assert_eq!(
            after_quiet, settling,
            "case {case:?}: a non-matching line must add no history row, got {after_quiet:?}"
        );

        // 5. What makes that silence a verdict rather than an absence: lines the SAME filter does
        //    match move the SAME alarm, through the state the aggregate of the values they publish
        //    implies under the declared comparison.
        let values: Vec<f64> = row.matching.iter().map(|(_, value)| *value).collect();
        let threshold = row
            .threshold
            .as_f64()
            .expect("every row spells its threshold as a number");
        let aggregated = aggregate(case, row.statistic, &values);
        let moved = if breaches(case, row.comparison, aggregated, threshold) {
            STATE_ALARM
        } else {
            STATE_OK
        };
        assert_ne!(
            moved, settled,
            "case {case:?}: the row must state matching lines that MOVE its alarm out of \
             {settled:?}, got an aggregate of {aggregated} against {threshold}"
        );
        moved_to(&router, row, moved).await;

        //    The values in the series are the ones the matching lines published and no others: the
        //    non-matching lines contributed no value of their own, not even a zero.
        let after_match = datapoints(&router, row).await;
        assert!(
            !after_match.is_empty(),
            "case {case:?}: the matching lines must publish into {}/{}, got no datapoint",
            row.namespace,
            row.metric
        );
        let smallest = values.iter().copied().fold(f64::INFINITY, f64::min);
        let largest = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        for datapoint in &after_match {
            assert_eq!(
                datapoint["Minimum"].as_f64(),
                Some(smallest),
                "case {case:?}: expected every window of the series to bottom out at the \
                 smallest value a matching line published ({smallest}), got {datapoint}"
            );
            assert_eq!(
                datapoint["Maximum"].as_f64(),
                Some(largest),
                "case {case:?}: expected every window of the series to top out at the largest \
                 value a matching line published ({largest}), got {datapoint}"
            );
        }

        //    And the move is recorded after the settling transition, with nothing between them:
        //    the only transitions the alarm has are the one its treatment owed and the one its
        //    matching lines drove.
        let mut wanted = settling.clone();
        wanted.push(transition(settled, moved));
        let full = history(&router, case, row.alarm, STATE_UPDATE).await;
        assert!(
            full.len() >= wanted.len() && full[..wanted.len()] == wanted[..],
            "case {case:?}: expected the deployed alarm's history to open with {wanted:?}, got \
             {full:?}"
        );
    }
}
