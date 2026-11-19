// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the member `DescribeKinesisStreamingDestination` reports the REASON
//! an enable could not be completed in: `KinesisDataStreamDestination` carries
//! `DestinationStatusDescription`, "the human-readable string that corresponds to the
//! replica status" (`smithy_query(query="shape",shape="KinesisDataStreamDestination")`).
//!
//! The rules pinned here:
//!
//! 1. every destination the reader reports in `DestinationStatus` `ENABLE_FAILED` carries a
//!    non-empty `DestinationStatusDescription`, naming the `StreamArn` the enable addressed
//!    — whichever of the conditions that resolve to no stream of the table's Region the
//!    enable ran into;
//! 2. the description states WHICH condition it was, so two enables that fail for different
//!    reasons are told apart by it and not only by the ARN they named;
//! 3. the member describes a FAILURE, so it is absent in every other status of the
//!    `DestinationStatus` enum (`ENABLING`, `ACTIVE`, `UPDATING`, `DISABLING`, `DISABLED`);
//! 4. it is stored state, not something a read mints: re-reading reports the same text, and
//!    an enable that succeeds from the failed record clears it.
//!
//! Every command and every read is driven over the wire (`POST /` with
//! `X-Amz-Target: <Service>.<OperationName>`) through the same `MultiServiceRouter` the
//! emulator's front doors use, with the DynamoDB and the Kinesis service mounted on ONE
//! shared engine, and nothing seeded through either store.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::contributor_insights::create_table;
use crate::emulator::{
    emulator, ensure_kinesis_stream, ok, ACTIVE_DEADLINE, POLL_INTERVAL, REGION,
};

/// The entry `table`'s destination for `arn` is reported as, or `Value::Null` when the
/// table holds no record for that stream.
async fn described(router: &MultiServiceRouter, table: &str, arn: &str) -> Value {
    ok(
        router,
        "DescribeKinesisStreamingDestination",
        json!({"TableName": table}),
    )
    .await["KinesisDataStreamDestinations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|destination| destination["StreamArn"] == json!(arn))
        .unwrap_or(Value::Null)
}

/// Start replication of `table` to `arn`, asserting the command is accepted.
async fn enable(router: &MultiServiceRouter, table: &str, arn: &str) -> Value {
    ok(
        router,
        "EnableKinesisStreamingDestination",
        json!({"TableName": table, "StreamArn": arn}),
    )
    .await
}

/// Poll `DescribeKinesisStreamingDestination` until `table`'s destination for `arn` reports
/// `status`, answering the entry it reported then — the waiter a client runs after an
/// enable, which "doesn't return results immediately". The wait is bounded by the suite's
/// generous deadline, so a destination that never settles fails naming the last entry it
/// gave rather than hanging.
async fn destination_when(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    arn: &str,
    status: &str,
) -> Value {
    let started = std::time::Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < ACTIVE_DEADLINE {
        let entry = described(router, table, arn).await;
        if entry["DestinationStatus"] == json!(status) {
            return entry;
        }
        last = entry;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "case {case:?}: the destination for {arn} never reached {status} within \
         {ACTIVE_DEADLINE:?}, last {last}"
    )
}

/// The description reported for an entry, or `"<absent>"` when the member is not there —
/// so an assertion names what the reader actually served.
fn description(entry: &Value) -> String {
    entry["DestinationStatusDescription"]
        .as_str()
        .unwrap_or("<absent>")
        .to_string()
}

/// Rule 1: whichever way the `StreamArn` resolves to no stream this table could replicate
/// to, the failed destination describes why, over the value the enable named.
#[tokio::test]
async fn every_failed_destination_describes_why_over_the_stream_arn_it_named() {
    let router = emulator();
    // Rows differ along WHY the value resolves to no stream of the table's Region: it is no
    // ARN at all, it names another service's resource, another resource type of Kinesis, a
    // stream of another Region, and a stream of this Region that nothing created. Every
    // value clears the modelled 37..=1024 length bound of `StreamArn`.
    let cases: Vec<(&str, String)> = vec![
        (
            "a value that is no ARN",
            "not-an-arn-at-all-but-past-the-length-floor".to_string(),
        ),
        (
            "another service's resource",
            engine::emulator_arn("dynamodb", REGION, "table/Elsewhere"),
        ),
        (
            "another resource type of Kinesis",
            engine::emulator_arn("kinesis", REGION, "consumer/live"),
        ),
        (
            "a stream of another Region",
            engine::emulator_arn("kinesis", "ap-south-1", "stream/peer-cdc"),
        ),
        (
            "a stream nothing created",
            engine::emulator_arn("kinesis", REGION, "stream/never-created"),
        ),
    ];

    for (index, (case, arn)) in cases.into_iter().enumerate() {
        // A table replicates to at most one Kinesis data stream, so each row gets its own.
        let table = format!("Described{index}");
        create_table(&router, &table, &[]).await;
        enable(&router, &table, &arn).await;

        let entry = destination_when(&router, case, &table, &arn, "ENABLE_FAILED").await;
        let reported = description(&entry);
        assert!(
            !reported.is_empty() && reported != "<absent>",
            "case {case:?}: a failed destination must carry a non-empty \
             DestinationStatusDescription, got {entry}"
        );
        assert!(
            reported.contains(&arn),
            "case {case:?}: the description must name the StreamArn the enable addressed \
             ({arn}), got {reported:?}"
        );
    }
}

/// Rule 2: the description tells the conditions apart. The two ARNs below differ only in
/// their Region field, so a description that carried no reason — only the ARN — would read
/// the same for both once that one difference is taken out.
#[tokio::test]
async fn the_description_tells_the_failure_conditions_apart() {
    let router = emulator();
    let shared = "stream/same-name";
    let peer = engine::emulator_arn("kinesis", "ap-south-1", shared);
    let absent = engine::emulator_arn("kinesis", REGION, shared);

    let mut templates = Vec::new();
    for (index, (case, arn)) in [
        ("a stream of another Region", &peer),
        ("a stream nothing created", &absent),
    ]
    .into_iter()
    .enumerate()
    {
        let table = format!("TellApart{index}");
        create_table(&router, &table, &[]).await;
        enable(&router, &table, arn).await;
        let entry = destination_when(&router, case, &table, arn, "ENABLE_FAILED").await;
        // The ARN itself is taken out, so what is left is the reason alone.
        templates.push((case, description(&entry).replace(arn.as_str(), "<arn>")));
    }

    assert_ne!(
        templates[0].1, templates[1].1,
        "case \"two conditions, one stream name\": {:?} and {:?} must describe different \
         reasons, got {:?} and {:?}",
        templates[0].0, templates[1].0, templates[0].1, templates[1].1
    );
}

/// Rules 3 and 4: the member is reported in the failed status and in no other one, it does
/// not change when it is read again, and an enable that succeeds clears it.
#[tokio::test]
async fn the_description_belongs_to_the_failed_status_alone() {
    let router = emulator();
    let live = ensure_kinesis_stream(&router, "live").await;
    create_table(&router, "Walked", &[]).await;

    // The statuses a destination that REPLICATES moves through: the transient status each
    // command leaves it in, read straight after the command, and the settled one after it.
    // No row of this walk describes a failure, because none of them is one.
    enable(&router, "Walked", &live).await;
    let mut walked = vec![("just enabled", described(&router, "Walked", &live).await)];
    walked.push((
        "replicating",
        destination_when(&router, "replicating", "Walked", &live, "ACTIVE").await,
    ));
    ok(
        &router,
        "UpdateKinesisStreamingDestination",
        json!({
            "TableName": "Walked",
            "StreamArn": live.clone(),
            "UpdateKinesisStreamingConfiguration": {
                "ApproximateCreationDateTimePrecision": "MICROSECOND"
            },
        }),
    )
    .await;
    walked.push((
        "just updated",
        described(&router, "Walked", &live).await,
    ));
    walked.push((
        "updated",
        destination_when(&router, "updated", "Walked", &live, "ACTIVE").await,
    ));
    ok(
        &router,
        "DisableKinesisStreamingDestination",
        json!({"TableName": "Walked", "StreamArn": live.clone()}),
    )
    .await;
    walked.push((
        "just stopped",
        described(&router, "Walked", &live).await,
    ));
    walked.push((
        "stopped",
        destination_when(&router, "stopped", "Walked", &live, "DISABLED").await,
    ));

    for (case, entry) in &walked {
        assert_eq!(
            entry["DestinationStatusDescription"],
            Value::Null,
            "case {case:?}: a destination in {} describes no failure, got {entry}",
            entry["DestinationStatus"]
        );
        assert_ne!(
            entry["DestinationStatus"],
            json!("ENABLE_FAILED"),
            "case {case:?}: a destination whose stream resolves never fails, got {entry}"
        );
    }

    // Replication has stopped, so the table takes an enable again — this one for a stream
    // that does not exist yet, which is the row that DOES describe a failure.
    let retried = engine::emulator_arn("kinesis", REGION, "stream/created-later");
    enable(&router, "Walked", &retried).await;
    let failed = destination_when(&router, "the retry", "Walked", &retried, "ENABLE_FAILED").await;
    let first = description(&failed);
    assert!(
        !first.is_empty() && first != "<absent>",
        "case \"the failed record\": expected a non-empty description, got {failed}"
    );
    // Rule 4: the text is stored, so reading it again reports the same thing, and the
    // reads changed neither the status nor the description.
    for read in 1..=3 {
        let again = described(&router, "Walked", &retried).await;
        assert_eq!(
            (
                again["DestinationStatus"].clone(),
                description(&again).as_str()
            ),
            (json!("ENABLE_FAILED"), first.as_str()),
            "case \"re-read {read}\": a read reports the stored failure unchanged, got {again}"
        );
    }

    // Once the stream exists the same enable registers the record again and it replicates:
    // there is no failure left to describe.
    ensure_kinesis_stream(&router, "created-later").await;
    enable(&router, "Walked", &retried).await;
    let replicating =
        destination_when(&router, "after the retry", "Walked", &retried, "ACTIVE").await;
    assert_eq!(
        replicating["DestinationStatusDescription"],
        Value::Null,
        "case \"after the retry\": a destination that replicates describes no failure, got \
         {replicating}"
    );
}
