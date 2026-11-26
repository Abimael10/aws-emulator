// SPDX-License-Identifier: CC0-1.0

//! What a step's `FailureDetails$LogFile` points at, read while the delivery of those files is
//! still going on.
//!
//! THE RULE UNDER TEST, one level above the recorded case:
//!
//! > "The path to the log file where the step failure root cause was originally recorded"
//! > (`FailureDetails$LogFile`) is a path a reader can FETCH: a step reports one only once its
//! > log files have really landed in the destination its cluster's `LogUri` named, so at every
//! > instant a read reports one the object store holds that object — and a destination nothing
//! > could be written to (a bucket the object store does not hold, a value naming no bucket at
//! > all, no `LogUri` at all) leaves the member absent however many times the step is read,
//! > because no such file exists.
//!
//! Why this suite exists beside [`crate::step_log_delivery`], which pins the files a finished
//! step leaves and the absence of them for a destination that does not resolve: that suite reads
//! the settled step, and the delivery of a step's files is work owed by the CLOCK that whichever
//! call reports the cluster next performs — including the cluster's own background work driver.
//! So the member can be read WHILE a delivery is in flight, and a reader that is handed a path
//! at that instant fetches the object or does not. This suite reads it exactly there: every row
//! drives CONCURRENT readers of the same cluster for the whole of its life and fetches every
//! path any of them reports, so a step that reports a file before the write has landed fails
//! here rather than handing a client a key nothing holds.
//!
//! The rows differ along what the rule generalises over: WHERE the files were to go — a bucket
//! the object store holds, a bucket it does not, a `LogUri` that names no bucket at all, and no
//! `LogUri` at all — and so whether a log file comes to exist for the step to report.
//!
//! Amazon S3 is mounted BESIDE Amazon EMR on one engine and every object is read through S3's
//! own `GetObject`/`ListObjectsV2`, so the only path that passes is one a client's own read
//! finds. The wiring, the launch and the two services are [`crate::step_log_delivery`]'s, so the
//! two suites cannot disagree about the cluster under test.

use std::time::Instant;

use serde_json::{json, Value};
use service_com_amazonaws_emr_elasticmapreduce::state::STDERR_LOG_OBJECT;
use smithy_server_runtime::MultiServiceRouter;

use crate::step_log_delivery::{
    emr_ok, emulator, keys_under, launch, put_object, s3, BUCKET, DEADLINE, MARKER, POLL_INTERVAL,
    TERMINAL,
};

/// How many readers of the same cluster look at once. More than one, because what a reader can
/// be handed is decided by what another caller's delivery is doing at that instant: three
/// `ListSteps` and the `DescribeCluster` beside them, each of which performs the delivery that
/// is owed.
const CONCURRENT_READERS: usize = 4;

/// One row: where the cluster's files were to go, and whether a log file comes to exist.
struct Row {
    case: &'static str,
    /// The `LogUri` the launch names, or `None` for a launch that names none.
    log_uri: Option<&'static str>,
    /// Whether the step comes to report a log file at all.
    reported: bool,
}

/// The table the rule is asserted over.
const ROWS: &[Row] = &[
    Row {
        case: "a destination the object store holds",
        log_uri: Some("s3://emr-step-logs/logs/"),
        reported: true,
    },
    Row {
        case: "a destination naming a bucket the object store does not hold",
        log_uri: Some("s3://emr-step-logs-no-such-bucket/logs/"),
        reported: false,
    },
    Row {
        case: "a value that names no bucket at all",
        log_uri: Some("not-an-s3-uri/logs/"),
        reported: false,
    },
    Row {
        case: "no LogUri at all",
        log_uri: None,
        reported: false,
    },
];

/// The log file one read reported for the first step of `listed`, or `None` when it reported
/// none.
fn reported_log_file(listed: &Value) -> Option<String> {
    listed["Steps"]
        .as_array()
        .and_then(|steps| steps.last())
        .and_then(|step| step["Status"]["FailureDetails"]["LogFile"].as_str())
        .map(str::to_string)
}

/// Whether the first step of `listed` has reached a state it never leaves.
fn settled(listed: &Value) -> bool {
    listed["Steps"]
        .as_array()
        .and_then(|steps| steps.last())
        .and_then(|step| step["Status"]["State"].as_str())
        .is_some_and(|state| TERMINAL.contains(&state))
}

/// The `s3://bucket/key` a reported log file names, split into the two halves a fetch needs.
fn addressed(reported: &str) -> Option<(String, String)> {
    let rest = reported.strip_prefix("s3://")?;
    let (bucket, key) = rest.split_once('/')?;
    (!bucket.is_empty() && !key.is_empty()).then(|| (bucket.to_string(), key.to_string()))
}

/// Fetch the object a reported log file names, failing the row when nothing holds it.
///
/// This is the rule's own assertion: a path handed to a client is one that client can fetch, so
/// a member reported before the write landed fails here.
async fn fetchable(router: &MultiServiceRouter, case: &str, reported: &str) {
    let (bucket, key) = addressed(reported).unwrap_or_else(|| {
        panic!("case {case:?}: expected a reported log file to name a bucket and a key, got {reported:?}")
    });
    let (status, body) = s3(router, "GET", &format!("/{bucket}/{key}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: the step reported the log file {reported:?}, but fetching it answered \
         {status} with {:?} — nothing points a reader at an object the store does not hold",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(
        body.get(..2),
        Some([0x1f, 0x8b].as_slice()),
        "case {case:?}: expected {reported:?} to be the gzip stream a delivered file is, got \
         {:?}",
        body.get(..2)
    );
}

/// RULE: a step reports a log file only once that file exists, so every path a read hands a
/// client is one the client can fetch; a destination nothing could be written to leaves the
/// member absent.
#[tokio::test]
async fn a_reported_step_log_file_is_a_path_the_object_store_holds() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let (status, _) = s3(&router, "PUT", &format!("/{BUCKET}"), &[]).await;
        assert!(
            (200..300).contains(&status),
            "case {case:?}: CreateBucket must succeed, got {status}"
        );
        // An application that RAISES, so the step fails and its status carries the
        // `FailureDetails` the log file is reported in at all.
        let key = format!("applications/reported-{index}.py");
        put_object(
            &router,
            case,
            &key,
            format!("raise RuntimeError(\"{MARKER}-reported-{index}\")\n").as_bytes(),
        )
        .await;
        let cluster_id = launch(
            &router,
            case,
            json!([{
                "Name": case,
                "ActionOnFailure": "CONTINUE",
                "HadoopJarStep": {
                    "Jar": "command-runner.jar",
                    "Args": ["spark-submit", format!("s3://{BUCKET}/{key}")],
                },
            }]),
            row.log_uri,
        )
        .await;

        // Readers of the same cluster, concurrently and for the whole of its life: the delivery
        // is performed by whichever call reports the cluster next — this service's own work
        // driver included — so this is where a reader can be handed a path whose write has not
        // landed. Every path any of them reports is fetched at once.
        let deadline = Instant::now() + DEADLINE;
        let mut ever_reported: Option<String> = None;
        loop {
            let listing = json!({ "ClusterId": cluster_id });
            // `CONCURRENT_READERS` calls in flight together, each of which drains what the
            // clock owes: the three readings of the steps are what can be handed a path, and
            // the cluster read beside them is another caller delivering at the same instant.
            let (first, second, third, fourth) = ::tokio::join!(
                emr_ok(&router, "ListSteps", listing.clone()),
                emr_ok(&router, "ListSteps", listing.clone()),
                emr_ok(&router, "ListSteps", listing.clone()),
                emr_ok(&router, "DescribeCluster", listing.clone()),
            );
            let answered: Vec<Value> = vec![first, second, third, fourth];
            assert_eq!(
                answered.len(),
                CONCURRENT_READERS,
                "case {case:?}: every reader of this look answers"
            );

            for listed in &answered {
                if let Some(reported) = reported_log_file(listed) {
                    fetchable(&router, case, &reported).await;
                    ever_reported = Some(reported);
                }
            }
            // A row that owes a log file is settled only once it reports one; a row that owes
            // none is settled when its step is.
            let done = answered.iter().any(settled)
                && (!row.reported || ever_reported.is_some());
            if done {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the step to settle{}, last read {:?}",
                if row.reported {
                    " and to report its delivered log file"
                } else {
                    ""
                },
                answered.last()
            );
            ::tokio::time::sleep(POLL_INTERVAL).await;
        }

        match (row.reported, ever_reported.as_deref()) {
            (true, Some(reported)) => {
                assert!(
                    reported.ends_with(&format!("/{STDERR_LOG_OBJECT}")),
                    "case {case:?}: expected the reported log file to be the step's own \
                     standard-error file, got {reported:?}"
                );
                // And it is still fetchable once everything has settled, not only at the
                // instant it was first reported.
                fetchable(&router, case, reported).await;
            }
            (true, None) => unreachable!("the loop above only ends once such a row reported one"),
            (false, reported) => {
                assert_eq!(
                    reported, None,
                    "case {case:?}: a step whose files could not be written must report no log \
                     file, got {reported:?}"
                );
                // ... and nothing was written anywhere either: only the application this row
                // staged is in the bucket.
                let keys = keys_under(&router, "").await;
                assert_eq!(
                    keys,
                    vec![key.clone()],
                    "case {case:?}: expected no log file in the destination, got {keys:?}"
                );
            }
        }
    }
}
