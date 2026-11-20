// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the INCREMENTAL half of the export surface, and for the restorable
//! window a client picks an incremental export period out of.
//!
//! The rules pinned here:
//!
//! 1. while point-in-time recovery is on, the restorable window `DescribeContinuousBackups`
//!    reports WIDENS: its newest point advances with the clock and its oldest reaches the
//!    period the recovery was given, so a client that polls the window sees a range it can
//!    pick an export period out of — and the oldest point STAYS PUT once it is there, so a
//!    client can name the bound it just read;
//! 2. an incremental export is taken over a PERIOD of that window: both of its instants
//!    must lie inside the window, and the period must span at least the fifteen minutes
//!    DynamoDB holds an incremental export to — a shorter one is refused with
//!    `InvalidExportTimeException` and the fixed sentence that states the rule, and records
//!    nothing;
//! 3. the members of that period are resolved as their own docs say: an incremental export
//!    must state the specification and its start, its end defaults to the newest exportable
//!    point and its view type to `NEW_AND_OLD_IMAGES`, and a FULL export cannot be given a
//!    period at all;
//! 4. what an accepted incremental export WRITES is the table's own change history over
//!    that period, read from the versions its continuous backups kept: one record per item
//!    the period changed, carrying the images on either side of it — an item created inside
//!    the period with no old image, one removed inside it with no new image, one written
//!    back to the value it already held not at all — and nothing for a write that landed
//!    outside the period.
//!
//! Every DynamoDB and Amazon S3 call is driven over the wire through the same
//! `MultiServiceRouter` the emulator's front doors use, and each rule is asserted over a
//! SET of inputs — the recovery period, the length of the export period, the member left
//! out, the kind of change the period made — rather than one example. Rule 4's history is
//! given to the stored table at instants in the past, the way a later request drives any
//! other clock-owed fact of this store, rather than waiting a quarter of an hour for a
//! period to become legal.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_dynamodb_dynamodb_20120810::ops::types::AttributeValue;
use service_com_amazonaws_dynamodb_dynamodb_20120810::state;

use crate::emulator::{
    call, create_bucket, describe_when_active, emulator, emulator_with_s3_and_engine, error_code,
    export_arn, message, ok, restorable_window, s3, POLL_INTERVAL, REGION,
};

/// The bucket every export of this suite writes into.
const BUCKET: &str = "incremental-table-exports";

/// A day in seconds, the unit `RecoveryPeriodInDays` counts in.
const DAY_SECONDS: f64 = 24.0 * 60.0 * 60.0;

/// The shortest period an incremental export may span, in seconds, and the sentence a
/// shorter one is refused with — both read from the store's own statement of the rule, so
/// the test pins the rule rather than a number copied beside it.
const MIN_PERIOD_SECONDS: f64 = state::MIN_INCREMENTAL_EXPORT_PERIOD.as_secs() as f64;
const TOO_SHORT: &str = state::INCREMENTAL_EXPORT_PERIOD_TOO_SHORT_MESSAGE;

/// The sentence every instant OUTSIDE the recovery window is refused with.
const INVALID_EXPORT_TIME: &str = state::INVALID_EXPORT_TIME_MESSAGE;

/// How long a poll for the window to widen, or for an export to finish, may take before
/// this suite calls it a hang. Generous slack over the emulator's own transient span: a
/// loaded machine is not a defect, and the point of the bound is that the wait ENDS rather
/// than how fast it does.
const DEADLINE: Duration = Duration::from_secs(60);

/// One on-demand table keyed on `Part`, settled `ACTIVE`.
async fn active_table(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, name, name).await;
}

/// Turn point-in-time recovery on for `name`, with the period when one is named.
async fn enable_recovery(router: &MultiServiceRouter, name: &str, period: Option<i64>) -> Value {
    let mut specification = json!({"PointInTimeRecoveryEnabled": true});
    if let Some(days) = period {
        specification["RecoveryPeriodInDays"] = json!(days);
    }
    ok(
        router,
        "UpdateContinuousBackups",
        json!({"TableName": name, "PointInTimeRecoverySpecification": specification}),
    )
    .await
}

/// Poll `DescribeContinuousBackups` until the window it reports spans `span` seconds, and
/// answer the window it then reports.
///
/// This is rule 1 under test: the window widens on its own as the recovery catches up with
/// the period it was given, so the condition becomes true without anything being written.
/// The deadline makes a window that never widens fail as the hang it is rather than looping
/// for ever.
async fn window_spanning(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    span: f64,
) -> (f64, f64) {
    let started = Instant::now();
    loop {
        let window = restorable_window(router, name).await;
        if window.1 - window.0 >= span {
            return window;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "case {case:?}: the restorable window never spanned {span} seconds, last window \
             was {window:?} spanning {}",
            window.1 - window.0
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// An `ExportTableToPointInTime` body for `table`, carrying the two required members and
/// the idempotency token every export of this surface is asked for under.
fn export_body(table: &str) -> Value {
    json!({
        "TableArn": state::table_arn(REGION, table),
        "S3Bucket": BUCKET,
        "ClientToken": fresh_token(),
    })
}

/// One token no other call of this suite uses.
fn fresh_token() -> String {
    static NEXT: ::std::sync::atomic::AtomicUsize = ::std::sync::atomic::AtomicUsize::new(0);
    format!(
        "incremental-exports-{}",
        NEXT.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed)
    )
}

/// An incremental export of `table` over `(from, to)`, in the view when one is named.
fn incremental_body(table: &str, from: Option<f64>, to: Option<f64>, view: Option<&str>) -> Value {
    let mut specification = json!({});
    if let Some(from) = from {
        specification["ExportFromTime"] = json!(from);
    }
    if let Some(to) = to {
        specification["ExportToTime"] = json!(to);
    }
    if let Some(view) = view {
        specification["ExportViewType"] = json!(view);
    }
    let mut body = export_body(table);
    body["ExportType"] = json!("INCREMENTAL_EXPORT");
    body["IncrementalExportSpecification"] = specification;
    body
}

/// The `ExportDescription` of either operation's response.
fn description(body: &Value) -> Value {
    body["ExportDescription"].clone()
}

/// The ARNs a listing of `table`'s exports reports, sorted so two listings compare as sets.
async fn listed_exports(router: &MultiServiceRouter, table: &str) -> Vec<String> {
    let listed = ok(
        router,
        "ListExports",
        json!({"TableArn": state::table_arn(REGION, table), "MaxResults": 25}),
    )
    .await;
    let mut arns: Vec<String> = listed["ExportSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|summary| summary["ExportArn"].as_str().map(str::to_string))
        .collect();
    arns.sort();
    arns
}

#[tokio::test]
async fn the_restorable_window_widens_until_it_spans_the_period_the_recovery_was_given() {
    let router = emulator();

    // Rows differ in the recovery period the table was given — none at all (the documented
    // default), each published bound of the modelled 1..=35 range, and a value inside it —
    // because the period is what the window widens UP TO. The second column is the period
    // the window must then span.
    let rows: [(&str, Option<i64>, i64); 4] = [
        ("no period named", None, 35),
        ("the smallest period", Some(1), 1),
        ("a period inside the range", Some(9), 9),
        ("the largest period", Some(35), 35),
    ];

    for (case, asked, expected) in rows {
        let name = format!("Widening{}", case.replace(' ', ""));
        active_table(&router, &name).await;
        let enabled = enable_recovery(&router, &name, asked).await;
        let recovery =
            &enabled["ContinuousBackupsDescription"]["PointInTimeRecoveryDescription"];
        assert_eq!(
            (
                recovery["PointInTimeRecoveryStatus"].clone(),
                recovery["RecoveryPeriodInDays"].clone(),
            ),
            (json!("ENABLED"), json!(expected)),
            "case {case:?}: the recovery reports itself on with the period it was given, \
             got {enabled}"
        );

        // The rule: the window reaches the period the recovery was given, which is at least
        // the shortest period an incremental export may span — so the quota a client is
        // held to is one it can satisfy.
        let depth = (expected as f64) * DAY_SECONDS;
        let (earliest, latest) = window_spanning(&router, case, &name, depth).await;
        let spread = latest - earliest;
        assert!(
            spread >= depth && spread >= MIN_PERIOD_SECONDS,
            "case {case:?}: the window spans the {expected}-day period it was given, got \
             {spread} seconds over ({earliest}, {latest})"
        );
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_secs_f64();
        assert!(
            earliest < latest && latest <= now,
            "case {case:?}: the window is ordered and its newest point trails the present, \
             got ({earliest}, {latest}) against {now}"
        );

        // And the oldest point STAYS PUT: a client that reads the bound and then names it
        // must find it still inside the window, which a bound sliding forward with the
        // clock could never be.
        let (again, _) = restorable_window(&router, &name).await;
        assert!(
            (again - earliest).abs() < 0.001,
            "case {case:?}: the oldest restorable point is the same one a moment later, got \
             {again} against {earliest}"
        );
    }
}

#[tokio::test]
async fn an_incremental_export_period_shorter_than_the_minimum_is_refused() {
    let (router, _engine) = emulator_with_s3_and_engine();
    create_bucket(&router, BUCKET).await;
    let table = "PeriodLength";
    active_table(&router, table).await;
    enable_recovery(&router, table, Some(35)).await;
    // The window has to span the periods below before any of them can be picked out of it.
    window_spanning(&router, "the periods under test", table, MIN_PERIOD_SECONDS * 4.0).await;

    // Rows differ in how long the period is, in seconds before the window's newest point:
    // the published minimum and periods either side of it, a period of no length at all and
    // one that runs backwards. The third column says whether the export is served.
    let rows: [(&str, f64, f64, bool); 9] = [
        ("a period of one minute", -60.0, 0.0, false),
        ("a period of one second", -1.0, 0.0, false),
        ("a period of no length at all", 0.0, 0.0, false),
        (
            "a period one second short of the minimum",
            -(MIN_PERIOD_SECONDS - 1.0),
            0.0,
            false,
        ),
        (
            "a period that runs backwards",
            -600.0,
            -1_200.0,
            false,
        ),
        (
            "the shortest period accepted",
            -MIN_PERIOD_SECONDS,
            0.0,
            true,
        ),
        ("a sixteen-minute period", -960.0, 0.0, true),
        ("a period of an hour", -3_600.0, 0.0, true),
        ("a period of a day", -DAY_SECONDS, 0.0, true),
    ];

    let mut served: Vec<String> = Vec::new();
    for (case, from_offset, to_offset, accepted) in rows {
        // The window as it stands for THIS row: its newest point advances with the clock, so
        // each period is placed against the window the call will be held to.
        let (_, latest) = restorable_window(&router, table).await;
        let body = incremental_body(
            table,
            Some(latest + from_offset),
            Some(latest + to_offset),
            None,
        );
        let (status, answered) = call(&router, "ExportTableToPointInTime", body).await;
        if accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: a period at or past the minimum is served, got {status} with \
                 {answered}"
            );
            let reported = description(&answered);
            assert!(
                reported["ExportStatus"] == json!("IN_PROGRESS")
                    || reported["ExportStatus"] == json!("COMPLETED"),
                "case {case:?}: the export it started is running or finished, got {reported}"
            );
            assert_eq!(
                reported["ExportType"],
                json!("INCREMENTAL_EXPORT"),
                "case {case:?}: the record is of the kind the request asked for, got {reported}"
            );
            served.push(export_arn(&answered));
            continue;
        }
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "InvalidExportTimeException"),
            "case {case:?}: a period shorter than the minimum is refused with the modelled \
             error, got {status} with {answered}"
        );
        assert_eq!(
            message(&answered),
            TOO_SHORT,
            "case {case:?}: the refusal states the fixed sentence of the fifteen-minute \
             rule, got {}",
            message(&answered)
        );
    }

    // A refused export records nothing: the catalogue holds exactly the served ones.
    served.sort();
    assert_eq!(
        listed_exports(&router, table).await,
        served,
        "the catalogue holds one export per served period and nothing for a refused one"
    );
}

#[tokio::test]
async fn an_incremental_period_outside_the_restorable_window_is_refused_for_the_time() {
    let (router, _engine) = emulator_with_s3_and_engine();
    create_bucket(&router, BUCKET).await;
    let table = "PeriodWindow";
    active_table(&router, table).await;
    enable_recovery(&router, table, Some(35)).await;
    window_spanning(&router, "the periods under test", table, MIN_PERIOD_SECONDS * 4.0).await;

    // Rows differ in WHICH end of the period falls outside the window, and by how far: both
    // instants are measured off the window the table publishes, so the rows hold whatever
    // the clock reads.
    type Period = fn(f64, f64) -> (f64, f64);
    let rows: [(&str, Period); 4] = [
        (
            "a period starting before the window reaches",
            |earliest, latest| (earliest - 60.0, latest),
        ),
        (
            "a period starting a day before the window reaches",
            |earliest, latest| (earliest - DAY_SECONDS, latest),
        ),
        (
            "a period ending in the future",
            |_, latest| (latest - 3_600.0, latest + 3_600.0),
        ),
        (
            "a period wholly in the future",
            |_, latest| (latest + 3_600.0, latest + 7_200.0),
        ),
    ];

    for (case, period_of) in rows {
        let (earliest, latest) = restorable_window(&router, table).await;
        let (from, to) = period_of(earliest, latest);
        let (status, answered) = call(
            &router,
            "ExportTableToPointInTime",
            incremental_body(table, Some(from), Some(to), None),
        )
        .await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "InvalidExportTimeException"),
            "case {case:?}: an instant outside the window is refused with the modelled \
             error, got {status} with {answered}"
        );
        assert_eq!(
            message(&answered),
            INVALID_EXPORT_TIME,
            "case {case:?}: the refusal states the fixed sentence of an instant no window \
             holds, got {}",
            message(&answered)
        );
    }

    // The control: a period the window DOES hold is served, so the rows above are refused
    // for the instants they named and not for the members being named at all.
    let (_, latest) = restorable_window(&router, table).await;
    let served = ok(
        &router,
        "ExportTableToPointInTime",
        incremental_body(table, Some(latest - 3_600.0), Some(latest), None),
    )
    .await;
    assert!(
        !export_arn(&served).is_empty(),
        "case \"a period inside the window\": the export is recorded, got {served}"
    );
}

#[tokio::test]
async fn the_members_of_an_incremental_period_are_held_to_their_own_rules() {
    let (router, _engine) = emulator_with_s3_and_engine();
    create_bucket(&router, BUCKET).await;
    let table = "PeriodMembers";
    active_table(&router, table).await;
    enable_recovery(&router, table, Some(35)).await;
    window_spanning(&router, "the members under test", table, MIN_PERIOD_SECONDS * 4.0).await;
    let (_, latest) = restorable_window(&router, table).await;

    // Rows differ in which member of the request is wrong: an incremental export with no
    // period at all, one whose period names no start, a period stated on a FULL export, and
    // a view type outside the set the model declares for it. Each is a fault of the
    // REQUEST, so each is the validation refusal such an input is — never a 501 and never a
    // record of an export nobody asked for.
    let rows: [(&str, Value); 4] = [
        (
            "an incremental export with no period",
            json!({"ExportType": "INCREMENTAL_EXPORT"}),
        ),
        (
            "a period naming no start",
            json!({"ExportType": "INCREMENTAL_EXPORT",
                   "IncrementalExportSpecification": {"ExportToTime": latest}}),
        ),
        (
            "a period stated on a full export",
            json!({"IncrementalExportSpecification": {
                "ExportFromTime": latest - 3_600.0, "ExportToTime": latest}}),
        ),
        (
            "a view type the model does not declare",
            json!({"ExportType": "INCREMENTAL_EXPORT",
                   "IncrementalExportSpecification": {
                       "ExportFromTime": latest - 3_600.0,
                       "ExportToTime": latest,
                       "ExportViewType": "OLD_IMAGE"}}),
        ),
    ];

    for (case, extra) in rows {
        let mut body = export_body(table);
        for (member, value) in extra.as_object().cloned().unwrap_or_default() {
            body[member] = value;
        }
        let (status, answered) = call(&router, "ExportTableToPointInTime", body).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {case:?}: a request this period cannot be read out of is refused as the \
             validation failure it is, got {status} with {answered}"
        );
    }

    // What the request LEAVES OUT and the operation documents a value for is resolved
    // rather than refused: "if this is not provided, the latest time with data available
    // will be used" (`$ExportToTime`), and the view type defaults to NEW_AND_OLD_IMAGES.
    let before = restorable_window(&router, table).await.1;
    let served = ok(
        &router,
        "ExportTableToPointInTime",
        incremental_body(table, Some(before - 2.0 * MIN_PERIOD_SECONDS), None, None),
    )
    .await;
    let after = restorable_window(&router, table).await.1;
    let reported = description(&served)["IncrementalExportSpecification"].clone();
    let resolved_to = reported["ExportToTime"].as_f64().unwrap_or_default();
    assert!(
        resolved_to >= before && resolved_to <= after,
        "case \"a period naming no end\": the end resolves to the newest exportable point, \
         got {resolved_to} outside ({before}, {after}) in {reported}"
    );
    assert_eq!(
        (
            reported["ExportViewType"].as_str().unwrap_or_default(),
            reported["ExportFromTime"].as_f64().unwrap_or_default(),
        ),
        ("NEW_AND_OLD_IMAGES", before - 2.0 * MIN_PERIOD_SECONDS),
        "case \"a period naming no view\": the view defaults to both images and the start \
         is the one stated, got {reported}"
    );
}

/// One write the stored table is given as of an instant in the past: the key it landed on,
/// and the value it stored (`None` for a delete).
struct Write {
    /// How many seconds before the suite's base instant the write landed.
    seconds_ago: u64,
    part: &'static str,
    value: Option<&'static str>,
}

/// Give `table` the history `writes` describes, as of instants in the past.
///
/// The table's continuous backups keep the version every write replaced, which is what an
/// incremental export of a period reads — so a history is given by making those writes
/// against the stored table, each at the instant it landed. Driving the instants rather
/// than waiting for them is what lets one test cover a period a quarter of an hour long:
/// the alternative is a test that sits out the clock.
fn give_history(
    engine: &Arc<engine::InMemoryEngine>,
    table_name: &str,
    base: SystemTime,
    recovery_started: SystemTime,
    writes: &[Write],
) {
    let state = state::service_state(&**engine).expect("the shared engine holds this store");
    let mut store = state.lock();
    let table = store
        .table_mut(REGION, table_name)
        .expect("the table this suite created");
    // The recovery this history was kept by, running since before the first of these
    // writes: a version is only kept while recovery is on.
    table
        .point_in_time_recovery
        .apply(true, Some(35), recovery_started);
    for write in writes {
        let at = base - Duration::from_secs(write.seconds_ago);
        let key = state::ItemKey {
            partition: state::AttributeValueKey::S(write.part.to_string()),
            sort: None,
        };
        let before = table.items.get(&key).cloned();
        table
            .point_in_time_recovery
            .record_item_version(at, &key, before.as_ref());
        match write.value {
            Some(value) => {
                let item: state::Item = HashMap::from([
                    ("Part".to_string(), AttributeValue::S(write.part.to_string())),
                    ("Val".to_string(), AttributeValue::S(value.to_string())),
                ]);
                table.items.insert(key, item);
            }
            None => {
                table.items.remove(&key);
            }
        }
    }
}

/// Poll `DescribeExport` until the export has finished, and answer the description it then
/// reports.
async fn described_when_finished(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < DEADLINE {
        last = description(&ok(router, "DescribeExport", json!({"ExportArn": arn})).await);
        assert_ne!(
            last["ExportStatus"],
            json!("FAILED"),
            "case {case:?}: the export had somewhere to write, got {last}"
        );
        if last["ExportStatus"] == json!("COMPLETED") {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the export never reported COMPLETED, last description {last}");
}

/// The records one export's data file holds, keyed by the `Part` of the item each is about.
///
/// The object is GZIP, one JSON document per line — the change records an incremental
/// export writes — and it is read from Amazon S3 itself, so what is asserted is what the
/// export really left in the bucket.
async fn records_in_data_file(
    router: &MultiServiceRouter,
    case: &str,
    export_id: &str,
) -> HashMap<String, Value> {
    use std::io::Read;

    let key = format!("AWSDynamoDB/{export_id}/data/{export_id}.json.gz");
    let read = s3(router)
        .get_object()
        .bucket(BUCKET)
        .key(&key)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: S3 serves {key:?}: {error:?}"));
    let bytes = read
        .body
        .collect()
        .await
        .expect("the object's bytes are read")
        .to_vec();
    let mut text = String::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .read_to_string(&mut text)
        .unwrap_or_else(|error| panic!("case {case:?}: the data file is GZIP, got {error:?}"));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let record: Value = serde_json::from_str(line).unwrap_or_else(|error| {
                panic!("case {case:?}: every data-file line is JSON, got {line:?}: {error:?}")
            });
            let part = record["Keys"]["Part"]["S"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a record names the keys of its item, got {record}")
                })
                .to_string();
            (part, record)
        })
        .collect()
}

#[tokio::test]
async fn an_incremental_export_writes_the_changes_the_period_made() {
    let (router, engine) = emulator_with_s3_and_engine();
    create_bucket(&router, BUCKET).await;
    let table = "ChangedItems";
    active_table(&router, table).await;

    // The suite's base instant, on a whole second so every instant below round-trips
    // through the wire's decimal form exactly.
    let base = UNIX_EPOCH
        + Duration::from_secs(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("the clock is after the epoch")
                .as_secs(),
        );
    let seconds_since_epoch = |instant: SystemTime| {
        instant
            .duration_since(UNIX_EPOCH)
            .expect("the instant is after the epoch")
            .as_secs_f64()
    };
    // The period under test: a quarter of an hour, ending ten minutes back so every write
    // of the history sits unambiguously on one side of one of its ends.
    let from = base - Duration::from_secs(1_500);
    let to = base - Duration::from_secs(600);

    // The history: the rows differ in WHERE each write falls relative to the period and in
    // what it did to the item — which is what decides whether the period changed that item
    // and which images the record carries.
    give_history(
        &engine,
        table,
        base,
        base - Duration::from_secs(2 * 60 * 60),
        &[
            // Before the period: the state it is measured from.
            Write { seconds_ago: 2_400, part: "untouched", value: Some("one") },
            Write { seconds_ago: 2_400, part: "updated", value: Some("one") },
            Write { seconds_ago: 2_400, part: "removed", value: Some("one") },
            Write { seconds_ago: 2_400, part: "restored", value: Some("one") },
            Write { seconds_ago: 2_400, part: "at-start", value: Some("one") },
            Write { seconds_ago: 2_400, part: "at-end", value: Some("one") },
            // Exactly at the period's INCLUSIVE start.
            Write { seconds_ago: 1_500, part: "at-start", value: Some("two") },
            // Inside the period.
            Write { seconds_ago: 1_440, part: "restored", value: Some("two") },
            Write { seconds_ago: 1_380, part: "restored", value: Some("one") },
            Write { seconds_ago: 1_200, part: "updated", value: Some("two") },
            Write { seconds_ago: 1_080, part: "removed", value: None },
            Write { seconds_ago: 1_020, part: "rewritten", value: Some("one") },
            Write { seconds_ago: 960, part: "rewritten", value: Some("two") },
            Write { seconds_ago: 900, part: "inserted", value: Some("one") },
            // Exactly at the period's EXCLUSIVE end, and after it.
            Write { seconds_ago: 600, part: "at-end", value: Some("two") },
            Write { seconds_ago: 300, part: "late", value: Some("one") },
        ],
    );

    // What the period did to each item: the images a record must carry, and `None` for an
    // item the period did not change at all.
    type Images = Option<(Option<&'static str>, &'static str)>;
    let expected: [(&str, &str, Images, &str); 9] = [
        (
            "an item changed inside the period",
            "updated",
            Some((Some("one"), "two")),
            "both images, since the item stood either side of the period",
        ),
        (
            "an item removed inside the period",
            "removed",
            Some((Some("one"), "")),
            "the old image alone, since the period left no item",
        ),
        (
            "an item created inside the period",
            "inserted",
            Some((None, "one")),
            "the new image alone, since there was no item to replace",
        ),
        (
            "an item written twice inside the period",
            "rewritten",
            Some((None, "two")),
            "one record carrying the value the period left",
        ),
        (
            "an item changed exactly at the period's start",
            "at-start",
            Some((Some("one"), "two")),
            "a record, since the start is inclusive",
        ),
        (
            "an item changed exactly at the period's end",
            "at-end",
            None,
            "no record, since the end is exclusive",
        ),
        (
            "an item written back to the value it held",
            "restored",
            None,
            "no record, since the period changed nothing",
        ),
        (
            "an item nothing wrote inside the period",
            "untouched",
            None,
            "no record",
        ),
        (
            "an item written after the period",
            "late",
            None,
            "no record",
        ),
    ];

    // Rows of the outer table: the two views an incremental export may be taken in, which
    // differ in whether a record carries the image the period REPLACED.
    for (case, view, old_images) in [
        ("both images", Some("NEW_AND_OLD_IMAGES"), true),
        ("the new image only", Some("NEW_IMAGE"), false),
        ("no view named, so both images", None, true),
    ] {
        let started = ok(
            &router,
            "ExportTableToPointInTime",
            incremental_body(
                table,
                Some(seconds_since_epoch(from)),
                Some(seconds_since_epoch(to)),
                view,
            ),
        )
        .await;
        let arn = export_arn(&started);
        let export_id = arn.rsplit('/').next().unwrap_or_default().to_string();
        let finished = described_when_finished(&router, case, &arn).await;
        let changed = expected
            .iter()
            .filter(|(_, _, images, _)| images.is_some())
            .count();
        assert_eq!(
            finished["ItemCount"],
            json!(changed),
            "case {case:?}: the export counts the items the period changed, got {finished}"
        );
        let reported = finished["IncrementalExportSpecification"].clone();
        assert_eq!(
            (
                reported["ExportFromTime"].as_f64().unwrap_or_default(),
                reported["ExportToTime"].as_f64().unwrap_or_default(),
                reported["ExportViewType"].as_str().unwrap_or_default(),
            ),
            (
                seconds_since_epoch(from),
                seconds_since_epoch(to),
                view.unwrap_or("NEW_AND_OLD_IMAGES"),
            ),
            "case {case:?}: the reader reports the period the export was taken over, got \
             {finished}"
        );

        let records = records_in_data_file(&router, case, &export_id).await;
        assert_eq!(
            records.len(),
            changed,
            "case {case:?}: the data file holds one record per changed item, got {records:?}"
        );
        for (row, part, images, expectation) in expected {
            let record = records.get(part);
            let Some((old, new)) = images else {
                assert!(
                    record.is_none(),
                    "case {case:?} / {row}: expected {expectation}, got {record:?}"
                );
                continue;
            };
            let Some(record) = record else {
                panic!("case {case:?} / {row}: expected {expectation}, got no record at all");
            };
            let expected_new = (!new.is_empty()).then(|| {
                json!({"Part": {"S": part}, "Val": {"S": new}})
            });
            assert_eq!(
                record.get("NewImage").cloned(),
                expected_new,
                "case {case:?} / {row}: expected {expectation}, got {record}"
            );
            let expected_old = old
                .filter(|_| old_images)
                .map(|old| json!({"Part": {"S": part}, "Val": {"S": old}}));
            assert_eq!(
                record.get("OldImage").cloned(),
                expected_old,
                "case {case:?} / {row}: expected {expectation}, got {record}"
            );
            // Each record says WHEN the change was written, in the epoch microseconds the
            // metadata of an incremental export's record carries.
            let written = record["Metadata"]["WriteTimestampMicros"]
                .as_str()
                .and_then(|micros| micros.parse::<u128>().ok())
                .unwrap_or_default();
            let period = (
                from.duration_since(UNIX_EPOCH).unwrap_or_default().as_micros(),
                to.duration_since(UNIX_EPOCH).unwrap_or_default().as_micros(),
            );
            assert!(
                written >= period.0 && written < period.1,
                "case {case:?} / {row}: the record says the change was written inside the \
                 period, got {written} against {period:?} in {record}"
            );
        }
    }

    // The table itself is untouched by the exports: a read is not a write, and an export
    // reads the history rather than rewriting it.
    let scanned = ok(
        &router,
        "Scan",
        json!({"TableName": table, "ConsistentRead": true}),
    )
    .await;
    assert_eq!(
        scanned["Count"].as_i64().unwrap_or_default(),
        8,
        "the source table still holds every item its history left, got {scanned}"
    );
}
