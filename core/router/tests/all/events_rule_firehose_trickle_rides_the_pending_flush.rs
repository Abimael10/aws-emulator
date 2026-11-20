// SPDX-License-Identifier: CC0-1.0

//! An EventBridge rule's Firehose target: an event published while the stream's buffer is
//! ALREADY filling rides the flush that buffer is already waiting for.
//!
//! The rule under test: `BufferingHints$IntervalInSeconds` is a property of the stream's
//! BUFFER and not of each record — "Buffer incoming data for the specified period of time, in
//! seconds, before delivering it to the destination", with "Firehose buffers data up to 5 MB
//! or for 5 minutes, whichever condition is satisfied first" (`CreateDeliveryStream`) stating
//! that interval as an UPPER bound on the wait rather than a delay charged to each arrival. So
//! the flush ending the interval that is ALREADY RUNNING writes one object holding every
//! record accepted since the previous flush, including one accepted moments before the
//! boundary: a record accepted into a filling buffer waits what is LEFT of that interval and
//! never a second one. Each flush is one object, so the objects split the arrival sequence
//! into consecutive runs — nothing delivered twice, nothing skipped, nothing reordered.
//!
//! EventBridge's part of the contract: for every target of a rule whose `EventPattern`
//! matched, it makes the call that target's service is invoked by — `firehose:PutRecord` for a
//! delivery stream — under the target's `RoleArn`, and the record then follows the stream's own
//! buffering contract, which is what every row below reads.
//!
//! What this adds to its two siblings. `firehose_buffer_flush_carries_mid_interval_arrivals`
//! pins the same flush rule for a DIRECT `PutRecord` (and this suite calls its wire helpers
//! rather than keeping copies of them); `firehose_buffer_interval_flush` publishes a trickle
//! through a matched rule's Firehose target and pins that every event reaches the destination
//! exactly once, but not WHICH flush carries which arrival. Here the two meet: the records
//! reach the buffer through a matched rule's target, and what each row reads is the composition
//! of the objects — so the whole EventBridge -> Firehose -> Amazon S3 path is held to the
//! mid-interval rule. The rows also span buffering hints neither sibling does: `SizeInMBs` at
//! its published minimum, at its published maximum, and left out for the documented default,
//! and a publication carrying several matching entries, which puts several records into the
//! filling buffer at once.
//!
//! Only a cross-service suite can pin it: the flow spans four services over ONE engine exactly
//! as `core/router/src/lib.rs` mounts them — EventBridge for the rules and the publications,
//! IAM for the roles both calls are made under, Firehose for the buffer and its flush cadence,
//! and Amazon S3 for the objects those flushes write. The emulator, the roles and the request
//! encoders are `events_rule_firehose_target_delivery`'s.
//!
//! No row asserts on elapsed time or sleeps for an outcome: the trickle IS a poll loop — each
//! turn publishes what each row's spacing is due for and re-reads every row's destination — and
//! what a row asserts is which records share an object. Nothing here depends on how fast the
//! host runs: a due turn publishes a PAIR of calls, moments apart, so two arrivals land in one
//! filling buffer however slowly the loop turns (a scheduler that re-armed its deadline per
//! arrival would put them in two objects whatever the gap), while the row's own spacing is what
//! spreads the arrivals that follow across the rest of the interval.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::events_rule_firehose_target_delivery::{emulator, given_role, send, REGION};
use super::firehose_buffer_flush_carries_mid_interval_arrivals::{
    body_of, json_call, keys_under,
};

/// The bucket every row delivers into; each row has its own `Prefix` under it.
const BUCKET: &str = "events-firehose-pending-flush";

/// The role every STREAM delivers under: Firehose has to be able to assume it, and it has to
/// allow the object write the delivery makes.
const DELIVERY_ROLE: &str = "pending-flush-delivery-role";

/// The role every TARGET is invoked under, and the call EventBridge makes on a delivery stream
/// that role therefore has to allow.
const TARGET_ROLE: &str = "pending-flush-target-role";
const PUT_RECORD_ACTION: &str = "firehose:PutRecord";

/// The `detail-type` every publication carries.
const DETAIL_TYPE: &str = "sim2real.test.trickle";

/// The base cadence of the trickle: one turn of the loop that publishes and re-reads every
/// destination. Each row publishes every `Row::ticks` turns of it, which is that row's own
/// arrival spacing — a property of the case, never a wait for an outcome.
const TICK: Duration = Duration::from_millis(50);

/// How many `PutEvents` calls one due turn makes for a row: a PAIR, moments apart, so two
/// arrivals are inside one filling buffer however slowly a loaded host turns the loop.
const CALLS_PER_TURN: usize = 2;

/// How long the whole trickle may run before a row fails: many times the longest interval any
/// row configures, so a loaded machine does not fail a row the clock would have served.
const TRICKLE_DEADLINE: Duration = Duration::from_secs(240);

/// How a row's publications reach its stream, and how long its buffer is held.
struct Row {
    case: &'static str,
    /// Names this row's stream, rule, destination prefix and markers, so rows cannot collide.
    name: &'static str,
    /// The `CreateDeliveryStream` member carrying this row's Amazon S3 destination.
    member: &'static str,
    /// `BufferingHints$IntervalInSeconds`, the interval whose running turn a late arrival has
    /// to ride.
    interval: i64,
    /// `BufferingHints$SizeInMBs`, or `None` to leave the member out so the documented default
    /// of 5 MiB stands. No row's envelopes come anywhere near either.
    size: Option<i64>,
    /// How many [`TICK`]s apart this row's publications are: its arrival spacing, well inside
    /// the row's own interval so arrivals keep landing in a filling buffer.
    ticks: u64,
    /// How many entries one `PutEvents` call carries, each matching the row's rule — so one
    /// call can put several records into the buffer at once.
    entries: usize,
}

/// The cases.
///
/// The rows differ along what the rule generalises over: the configured `IntervalInSeconds`
/// (several values inside the published range 0..900 — `smithy_query(query="shape",
/// shape="IntervalInSeconds")`), the configured `SizeInMBs` (its published minimum 1, its
/// published maximum 128, and the member left out for the documented default of 5), how far
/// apart the arrivals are relative to that interval, how many records one arrival puts, and
/// which destination member carries the Amazon S3 location.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "arrivals every tick into an extended destination buffering 1 s",
            name: "events-trickle-extended",
            member: "ExtendedS3DestinationConfiguration",
            interval: 1,
            size: Some(128),
            ticks: 1,
            entries: 1,
        },
        Row {
            case: "two-entry publications every other tick into a plain S3 destination at 2 s",
            name: "events-trickle-plain",
            member: "S3DestinationConfiguration",
            interval: 2,
            size: Some(1),
            ticks: 2,
            entries: 2,
        },
        Row {
            case: "arrivals four ticks apart with no SizeInMBs at all, buffering 3 s",
            name: "events-trickle-default-size",
            member: "ExtendedS3DestinationConfiguration",
            interval: 3,
            size: None,
            ticks: 4,
            entries: 1,
        },
    ]
}

/// One row under test while the trickle runs.
struct Leg {
    case: &'static str,
    name: &'static str,
    /// The `source` this row's rule matches on, which no other row publishes.
    source: String,
    entries: usize,
    ticks: u64,
    /// The marker of every record published so far, in publication order.
    markers: Vec<String>,
    /// Which `PutEvents` call each marker was published by, indexed as `markers` is.
    call_of: Vec<usize>,
    /// How many `PutEvents` calls this row has made.
    calls: usize,
    /// Which records each delivered object holds, by key: the run of publication indices its
    /// body carries, read once — an object is written by one flush and never rewritten.
    runs: BTreeMap<String, Vec<usize>>,
    /// The run of one object that was found to hold the records of MORE THAN ONE publication —
    /// the pending flush a later arrival rode — once such an object exists.
    rode: Option<Vec<usize>>,
}

impl Leg {
    /// This row's destination prefix.
    fn prefix(&self) -> String {
        format!("{}/", self.name)
    }

    /// The marker the `index`th record of this row carries, delimited at both ends so one
    /// marker is never a substring of another.
    fn marker(&self, index: usize) -> String {
        format!("<{}-{index}>", self.name)
    }
}

#[tokio::test]
async fn a_record_put_into_a_filling_buffer_rides_its_pending_flush() {
    let router = emulator();
    let rows = rows();
    given_bucket(&router).await;
    let delivery_role = given_role(
        &router,
        "setup",
        DELIVERY_ROLE,
        "firehose.amazonaws.com",
        Some("s3:PutObject"),
    )
    .await;
    let target_role = given_role(
        &router,
        "setup",
        TARGET_ROLE,
        "events.amazonaws.com",
        Some(PUT_RECORD_ACTION),
    )
    .await;

    // Every stream is created before any of them is used, so the suite spends ONE clock-owed
    // CREATING -> ACTIVE span rather than one per row.
    for row in &rows {
        given_stream(&router, row, &delivery_role).await;
    }
    let mut legs: Vec<Leg> = rows
        .iter()
        .map(|row| Leg {
            case: row.case,
            name: row.name,
            source: format!("com.demo.{}", row.name),
            entries: row.entries,
            ticks: row.ticks,
            markers: Vec::new(),
            call_of: Vec::new(),
            calls: 0,
            runs: BTreeMap::new(),
            rode: None,
        })
        .collect();
    for leg in &legs {
        await_active(&router, leg).await;
        given_rule(&router, leg, &target_role).await;
    }

    // The trickle. Each turn publishes what every row's own spacing is due for and then reads
    // every row's destination, so what a row asserts is read while its publications are still
    // coming. A row stops publishing once it has seen a flush that carried more than one of
    // its publications, and is done once every record it published has been delivered.
    let deadline = Instant::now() + TRICKLE_DEADLINE;
    let mut turn: u64 = 0;
    loop {
        for leg in &mut legs {
            if leg.rode.is_none() && turn.is_multiple_of(leg.ticks) {
                for _ in 0..CALLS_PER_TURN {
                    publish(&router, leg).await;
                }
            }
        }
        let mut pending: Vec<&str> = Vec::new();
        for leg in &mut legs {
            read_destination(&router, leg).await;
            let runs = runs(leg);
            if leg.rode.is_none() {
                let rode = runs
                    .iter()
                    .find(|run| publications(run.as_slice(), &leg.call_of) > 1)
                    .cloned();
                leg.rode = rode;
            }
            let delivered: usize = runs.iter().map(Vec::len).sum();
            if leg.rode.is_none() || delivered < leg.markers.len() {
                pending.push(leg.case);
            }
        }
        if pending.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected every row to see a flush carrying more than one of its publications, \
             and then every published record delivered; still pending: {pending:?}"
        );
        tokio::time::sleep(TICK).await;
        turn += 1;
    }

    // What every row ended up with: a flush that swept up several publications, every record
    // delivered exactly once, and fewer objects than records — the flushes were shared.
    for leg in &legs {
        // The trickle only ends once a flush was read that had swept up the records of more
        // than one publication, which is the ride itself; what is left to require of a row is
        // that the flushes between them account for everything it published.
        let runs = runs(leg);
        let delivered: usize = runs.iter().map(Vec::len).sum();
        assert_eq!(
            delivered,
            leg.markers.len(),
            "case {:?}: every published record is delivered exactly once, got {delivered} of \
             {} in the runs {runs:?}",
            leg.case,
            leg.markers.len()
        );
        assert!(
            runs.len() < leg.markers.len(),
            "case {:?}: records that arrived inside one interval share its flush, so there are \
             fewer objects than records; got {} object(s) for {} record(s)",
            leg.case,
            runs.len(),
            leg.markers.len()
        );
    }
}

/// How many distinct `PutEvents` calls the records of `run` came from.
fn publications(run: &[usize], call_of: &[usize]) -> usize {
    let mut calls: Vec<usize> = run.iter().map(|index| call_of[*index]).collect();
    calls.sort_unstable();
    calls.dedup();
    calls.len()
}

/// What `leg`'s delivered objects hold, as runs of publication indices — one run per object, in
/// key order.
///
/// Asserts the rule's own shape as it reads them: no two objects hold the same record, and
/// together they cover a PREFIX of what has been published. So a record that waited out a
/// second interval while a later one was delivered, and a record delivered twice, fail here;
/// [`read_destination`] has already required each object's own run to be consecutive.
fn runs(leg: &Leg) -> Vec<Vec<usize>> {
    let runs: Vec<Vec<usize>> = leg.runs.values().cloned().collect();
    let mut covered: Vec<usize> = runs.iter().flatten().copied().collect();
    covered.sort_unstable();
    let expected: Vec<usize> = (0..covered.len()).collect();
    assert_eq!(
        covered, expected,
        "case {:?}: the delivered objects cover the arrival sequence from its start, each \
         record once; got the runs {runs:?} of {} published",
        leg.case,
        leg.markers.len()
    );
    runs
}

/// Which of `markers` one object's body holds, as indices in the order the body carries them.
///
/// A marker found twice is reported twice, so an object holding one record of a buffer more
/// than once fails the reading rather than being counted once.
fn held(body: &str, markers: &[String]) -> Vec<usize> {
    let mut found: Vec<(usize, usize)> = Vec::new();
    for (index, marker) in markers.iter().enumerate() {
        let mut from = 0;
        while let Some(at) = body[from..].find(marker.as_str()) {
            found.push((from + at, index));
            from += at + marker.len();
        }
    }
    found.sort_unstable();
    found.into_iter().map(|(_, index)| index).collect()
}

/// Publish one `PutEvents` call for `leg`, carrying this row's entries, and record the markers
/// it put into the stream's buffer.
async fn publish(router: &MultiServiceRouter, leg: &mut Leg) {
    let call = leg.calls;
    let mut entries: Vec<Value> = Vec::new();
    for _ in 0..leg.entries {
        let marker = leg.marker(leg.markers.len() + entries.len());
        entries.push(json!({
            "Source": &leg.source,
            "DetailType": DETAIL_TYPE,
            "Detail": json!({ "marker": marker }).to_string(),
        }));
    }
    let answered = json_call(
        router,
        leg.case,
        "events",
        "AWSEvents.PutEvents",
        json!({ "Entries": entries }),
    )
    .await;
    assert_eq!(
        answered["FailedEntryCount"],
        json!(0),
        "case {:?}: the publication must be accepted whole, got {answered}",
        leg.case
    );
    for _ in 0..leg.entries {
        let index = leg.markers.len();
        leg.markers.push(leg.marker(index));
        leg.call_of.push(call);
    }
    leg.calls += 1;
}

/// Read `leg`'s destination: the records of every object it has not read yet.
///
/// An object is written by ONE flush and never rewritten, so its run is read once — and that
/// run has to be the records that arrived one after another with nothing in between, which is
/// what one flush writes.
async fn read_destination(router: &MultiServiceRouter, leg: &mut Leg) {
    let markers: Vec<String> = (0..leg.markers.len())
        .map(|index| leg.marker(index))
        .collect();
    for key in keys_under(router, leg.case, BUCKET, &leg.prefix()).await {
        if leg.runs.contains_key(&key) {
            continue;
        }
        let body = body_of(router, leg.case, BUCKET, &key).await;
        let held = held(&body, &markers);
        assert!(
            !held.is_empty(),
            "case {:?}: a delivered object holds the records of the flush that wrote it, got \
             none in {key:?}",
            leg.case
        );
        for pair in held.windows(2) {
            assert_eq!(
                pair[1],
                pair[0] + 1,
                "case {:?}: one flush writes the records accepted since the previous one, in \
                 arrival order and with nothing in between; {key:?} holds {held:?}",
                leg.case
            );
        }
        leg.runs.insert(key, held);
    }
}

/// The bucket every row delivers into.
async fn given_bucket(router: &MultiServiceRouter) {
    let (status, body) = send(
        router,
        "s3",
        "PUT",
        &format!("/{BUCKET}"),
        &[],
        format!(
            "<CreateBucketConfiguration><LocationConstraint>{REGION}\
             </LocationConstraint></CreateBucketConfiguration>"
        )
        .into_bytes(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "setup: the destination bucket must be created, got {status} {}",
        String::from_utf8_lossy(&body)
    );
}

/// Create `row`'s delivery stream, buffering as the row asks, without waiting for it.
async fn given_stream(router: &MultiServiceRouter, row: &Row, delivery_role: &str) {
    let mut hints = json!({ "IntervalInSeconds": row.interval });
    if let Some(size) = row.size {
        hints["SizeInMBs"] = json!(size);
    }
    let destination = json!({
        "RoleARN": delivery_role,
        "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
        "Prefix": format!("{}/", row.name),
        "CompressionFormat": "UNCOMPRESSED",
        "BufferingHints": hints,
    });
    let mut request = json!({
        "DeliveryStreamName": row.name,
        "DeliveryStreamType": "DirectPut",
    });
    request[row.member] = destination;
    json_call(
        router,
        row.case,
        "firehose",
        "Firehose_20150804.CreateDeliveryStream",
        request,
    )
    .await;
}

/// Wait until Firehose reports `leg`'s stream `ACTIVE`, which is when a put to it is accepted.
async fn await_active(router: &MultiServiceRouter, leg: &Leg) {
    let deadline = Instant::now() + TRICKLE_DEADLINE;
    loop {
        let described = json_call(
            router,
            leg.case,
            "firehose",
            "Firehose_20150804.DescribeDeliveryStream",
            json!({ "DeliveryStreamName": leg.name }),
        )
        .await;
        let status = described["DeliveryStreamDescription"]["DeliveryStreamStatus"].clone();
        if status == json!("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected the stream to reach ACTIVE, last saw {status:?}",
            leg.case
        );
        tokio::time::sleep(TICK).await;
    }
}

/// The rule that matches `leg`'s publications, with that row's stream as its only target.
async fn given_rule(router: &MultiServiceRouter, leg: &Leg, target_role: &str) {
    let pattern = json!({ "source": [&leg.source] }).to_string();
    json_call(
        router,
        leg.case,
        "events",
        "AWSEvents.PutRule",
        json!({ "Name": leg.name, "EventPattern": pattern, "State": "ENABLED" }),
    )
    .await;
    let answered = json_call(
        router,
        leg.case,
        "events",
        "AWSEvents.PutTargets",
        json!({
            "Rule": leg.name,
            "Targets": [{
                "Id": "firehose-target",
                "Arn": format!(
                    "arn:aws:firehose:{REGION}:{}:deliverystream/{}",
                    engine::emulator_account_id(),
                    leg.name
                ),
                "RoleArn": target_role,
            }],
        }),
    )
    .await;
    assert_eq!(
        answered["FailedEntryCount"],
        json!(0),
        "case {:?}: PutTargets reported a failed entry: {answered}",
        leg.case
    );
}
