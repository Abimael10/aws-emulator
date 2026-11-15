// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH members the three commands over a table's Kinesis
//! streaming destination answer with: `EnableKinesisStreamingDestination`,
//! `UpdateKinesisStreamingDestination` and `DisableKinesisStreamingDestination`.
//!
//! The rule pinned here: each command's 200 answer reports the destination it addressed —
//! the table, the stream and the destination's status — plus the configuration member for
//! the configuration THAT CALL PUT IN FORCE, and nothing else. A registration and a
//! configuration change each put one in force, so each carries its own configuration
//! member (`EnableKinesisStreamingConfiguration` and `UpdateKinesisStreamingConfiguration`
//! respectively); a stop puts none in force, so it answers with the three identity and
//! status members alone, whatever configuration the stopped destination replicated under
//! and whatever configuration member the stopping request itself carried. The member set
//! is a property of the command, never of the request's shape.
//!
//! The configuration the stopped destination replicated under is unchanged by the stop and
//! stays readable through `DescribeKinesisStreamingDestination`, which is the operation
//! that REPORTS a destination's configuration — so each row round-trips through it.
//!
//! The rule is asserted over a SET of inputs: the command called, the precision in force
//! when it runs (none, and each value of
//! `EnableKinesisStreamingConfiguration$ApproximateCreationDateTimePrecision`, whose value
//! set comes from
//! `smithy_query(query="shape",shape="ApproximateCreationDateTimePrecision")` — MILLISECOND
//! and MICROSECOND), the configuration member the request carries (absent, empty, and one
//! naming each precision), and whether the table is addressed by bare name or by its ARN.
//!
//! Every command and every read is driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same
//! `MultiServiceRouter` the emulator's front doors use, with nothing seeded through the
//! store. A command's transient span is waited out by POLLING the reader for the status it
//! settles on, never by sleeping a span.

use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_dynamodb_dynamodb_20120810::state;

use crate::contributor_insights::create_table;
use crate::emulator::{
    emulator, ensure_kinesis_stream, ok, ACTIVE_DEADLINE, POLL_INTERVAL, REGION,
};

/// The members the answers below are compared against, sorted, so a missing member and an
/// extra one are both named by the same assertion.
fn members(answer: &Value) -> Vec<String> {
    let mut named: Vec<String> = answer
        .as_object()
        .map(|body| body.keys().cloned().collect())
        .unwrap_or_default();
    named.sort();
    named
}

/// The destination the reader reports for `stream_arn`, or `Value::Null` when the table
/// reports none for that stream.
async fn described(router: &MultiServiceRouter, table: &str, stream_arn: &str) -> Value {
    let report = ok(
        router,
        "DescribeKinesisStreamingDestination",
        json!({"TableName": table}),
    )
    .await;
    report["KinesisDataStreamDestinations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|destination| destination["StreamArn"] == json!(stream_arn))
        .unwrap_or(Value::Null)
}

/// Poll the reader until the destination for `stream_arn` reports `wanted`, and answer
/// with the report that did. A command's span is the emulator's to owe, so the wait is for
/// the CONDITION and bounded only by the suite's generous deadline.
async fn destination_when(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    stream_arn: &str,
    wanted: &str,
) -> Value {
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < ACTIVE_DEADLINE {
        last = described(router, table, stream_arn).await;
        if last["DestinationStatus"] == json!(wanted) {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the destination never reported {wanted}, last saw {last}")
}

/// Register one destination replicating under `precision` and wait until it is `ACTIVE`.
async fn replicating(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    stream: &str,
    precision: Option<&str>,
) -> String {
    // The stream has to EXIST for the registration to replicate: a destination whose
    // `StreamArn` resolves to no stream settles `ENABLE_FAILED` rather than `ACTIVE`.
    let arn = ensure_kinesis_stream(router, stream).await;
    let mut request = json!({"TableName": table, "StreamArn": arn.clone()});
    if let Some(precision) = precision {
        request["EnableKinesisStreamingConfiguration"] =
            json!({"ApproximateCreationDateTimePrecision": precision});
    }
    ok(router, "EnableKinesisStreamingDestination", request).await;
    destination_when(router, case, table, &arn, "ACTIVE").await;
    arn
}

/// One row of the stop table: the case, the precision in force when the stop runs, the
/// `EnableKinesisStreamingConfiguration` member the stopping request carries, and whether
/// the table is addressed by its ARN rather than its bare name.
type StopRow = (&'static str, Option<&'static str>, Option<Value>, bool);

/// A stop reports the destination's identity and status ONLY: it puts no configuration in
/// force, so its answer carries no configuration member at all.
#[tokio::test]
async fn a_stop_answers_with_the_identity_and_status_members_only() {
    let rows: Vec<StopRow> = vec![
        ("no precision in force", None, None, false),
        ("millisecond in force", Some("MILLISECOND"), None, false),
        ("microsecond in force", Some("MICROSECOND"), None, false),
        (
            "microsecond in force, addressed by the table's ARN",
            Some("MICROSECOND"),
            None,
            true,
        ),
        (
            "millisecond in force, the request carries an empty configuration",
            Some("MILLISECOND"),
            Some(json!({})),
            false,
        ),
        (
            "millisecond in force, the request names the other precision",
            Some("MILLISECOND"),
            Some(json!({"ApproximateCreationDateTimePrecision": "MICROSECOND"})),
            false,
        ),
        (
            "no precision in force, the request names one",
            None,
            Some(json!({"ApproximateCreationDateTimePrecision": "MILLISECOND"})),
            false,
        ),
    ];

    for (index, (case, precision, configuration, by_arn)) in rows.into_iter().enumerate() {
        let router = emulator();
        let table = format!("Stop{index}");
        create_table(&router, &table, &[]).await;
        let arn = replicating(&router, case, &table, "cdc", precision).await;
        let addressed = if by_arn {
            state::table_arn(REGION, &table)
        } else {
            table.clone()
        };

        let mut request = json!({"TableName": addressed, "StreamArn": arn.clone()});
        if let Some(configuration) = configuration {
            request["EnableKinesisStreamingConfiguration"] = configuration;
        }
        let answered = ok(&router, "DisableKinesisStreamingDestination", request).await;

        let named = members(&answered);
        let wanted = vec![
            "DestinationStatus".to_string(),
            "StreamArn".to_string(),
            "TableName".to_string(),
        ];
        assert_eq!(
            named, wanted,
            "case {case:?}: expected the members {wanted:?}, got {named:?} in {answered}"
        );
        assert_eq!(
            answered["TableName"],
            json!(table),
            "case {case:?}: the answer names the table it changed, got {answered}"
        );
        assert_eq!(
            answered["StreamArn"],
            json!(arn),
            "case {case:?}: the answer names the stream it stopped replicating to, got {answered}"
        );
        assert_eq!(
            answered["DestinationStatus"],
            json!("DISABLING"),
            "case {case:?}: expected the transient status, got {answered}"
        );

        // The configuration the destination replicated under is unchanged by the stop, and
        // the reader — the operation that REPORTS a destination's configuration — still
        // reports it once replication has stopped.
        let stopped = destination_when(&router, case, &table, &arn, "DISABLED").await;
        assert_eq!(
            stopped["ApproximateCreationDateTimePrecision"],
            precision.map_or(Value::Null, |precision| json!(precision)),
            "case {case:?}: expected the reader to still report {precision:?}, got {stopped}"
        );
    }
}

/// One row of the configuration-command table: the case, the command, the precision in
/// force before it, the configuration member it carries, the member its answer reports the
/// configuration under, the configuration that answer reports, and the transient status.
type ConfiguredRow = (
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<Value>,
    &'static str,
    Value,
    &'static str,
);

/// The commands that DO put a configuration in force report it: a registration under
/// `EnableKinesisStreamingConfiguration` and a configuration change under
/// `UpdateKinesisStreamingConfiguration`, each beside the same three identity and status
/// members.
#[tokio::test]
async fn a_command_that_puts_a_configuration_in_force_reports_it() {
    let rows: Vec<ConfiguredRow> = vec![
        (
            "a registration naming millisecond",
            "EnableKinesisStreamingDestination",
            None,
            Some(json!({"ApproximateCreationDateTimePrecision": "MILLISECOND"})),
            "EnableKinesisStreamingConfiguration",
            json!({"ApproximateCreationDateTimePrecision": "MILLISECOND"}),
            "ENABLING",
        ),
        (
            "a registration naming microsecond",
            "EnableKinesisStreamingDestination",
            None,
            Some(json!({"ApproximateCreationDateTimePrecision": "MICROSECOND"})),
            "EnableKinesisStreamingConfiguration",
            json!({"ApproximateCreationDateTimePrecision": "MICROSECOND"}),
            "ENABLING",
        ),
        (
            "a configuration change to microsecond",
            "UpdateKinesisStreamingDestination",
            Some("MILLISECOND"),
            Some(json!({"ApproximateCreationDateTimePrecision": "MICROSECOND"})),
            "UpdateKinesisStreamingConfiguration",
            json!({"ApproximateCreationDateTimePrecision": "MICROSECOND"}),
            "UPDATING",
        ),
        (
            "a configuration change to millisecond",
            "UpdateKinesisStreamingDestination",
            Some("MICROSECOND"),
            Some(json!({"ApproximateCreationDateTimePrecision": "MILLISECOND"})),
            "UpdateKinesisStreamingConfiguration",
            json!({"ApproximateCreationDateTimePrecision": "MILLISECOND"}),
            "UPDATING",
        ),
    ];

    for (index, (case, operation, in_force, carried, member, reported, status)) in
        rows.into_iter().enumerate()
    {
        let router = emulator();
        let table = format!("Configured{index}");
        create_table(&router, &table, &[]).await;
        let arn = if operation == "EnableKinesisStreamingDestination" {
            ensure_kinesis_stream(&router, "cdc").await
        } else {
            replicating(&router, case, &table, "cdc", in_force).await
        };

        let mut request = json!({"TableName": table.clone(), "StreamArn": arn.clone()});
        if let Some(carried) = carried {
            request[member] = carried;
        }
        let answered = ok(&router, operation, request).await;

        let named = members(&answered);
        let mut wanted = vec![
            "DestinationStatus".to_string(),
            "StreamArn".to_string(),
            "TableName".to_string(),
            member.to_string(),
        ];
        wanted.sort();
        assert_eq!(
            named, wanted,
            "case {case:?}: expected the members {wanted:?}, got {named:?} in {answered}"
        );
        assert_eq!(
            answered[member], reported,
            "case {case:?}: expected the configuration put in force to be {reported}, got \
             {answered}"
        );
        assert_eq!(
            answered["DestinationStatus"],
            json!(status),
            "case {case:?}: expected the transient status {status}, got {answered}"
        );

        // The configuration the command put in force is what the reader reports once the
        // command has settled.
        let settled = destination_when(&router, case, &table, &arn, "ACTIVE").await;
        assert_eq!(
            settled["ApproximateCreationDateTimePrecision"],
            reported["ApproximateCreationDateTimePrecision"],
            "case {case:?}: expected the reader to report the configuration in force, got \
             {settled}"
        );
    }
}
