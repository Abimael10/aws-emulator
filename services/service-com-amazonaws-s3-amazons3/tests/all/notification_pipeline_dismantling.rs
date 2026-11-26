// SPDX-License-Identifier: CC0-1.0

//! What a bucket's notification configuration DOES once a client takes the pipeline it drives
//! APART: the leg every event-driven estate ends with, and the one a client walks when it
//! retires a trigger while the bucket goes on serving writes.
//!
//! RULE: a bucket publishes an object event to a Lambda destination only while the recorded
//! configuration still names that destination FOR THAT EVENT CLASS and the destination is still
//! there to take the publication — so a write made after the configuration was narrowed to
//! another class, after it was cleared with the empty container, or after the named function was
//! deleted is accepted exactly as before and published to nobody, while the configuration
//! `GetBucketNotificationConfiguration` reports stays exactly what was last recorded and every
//! object the bucket already holds stays readable. The estate can then be removed in the order a
//! client removes it — the bucket emptied through its own listing, then the bucket and the
//! execution role deleted — each read-back reporting the removal.
//!
//! The sibling suite [`crate::notification_lambda_handler_run`] pins the other half for an ARMED
//! configuration (which keys its filter admits reach the function at all) and owns the emulator,
//! the deployment package and the converting handler this suite drives; what only this suite
//! reaches is what happens to the delivery when a part of the pipeline GOES AWAY, which is the
//! leg nothing else asserts: an emulator that went on publishing to a cleared configuration, or
//! that refused the write once its destination was gone, would pass every other notification
//! suite in this crate.
//!
//! The rows differ along what the rule generalises over: WHICH part of the pipeline is taken
//! away — the event class the configuration covers, the configuration itself, and the
//! destination the configuration names — and each row reads its absence only after a positive
//! control written through the live pipeline has been converted, so an absence is the delivery
//! Amazon S3 did not make and never lateness.
//!
//! Amazon S3, Lambda, IAM and the token service are mounted on ONE engine, exactly as
//! `core/router/src/lib.rs` mounts them, so the bucket this suite reads is the bucket the
//! handler wrote.

use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};

use crate::bucket_lifecycle::elements_of;
use crate::continuation_token_alphabet::handed_back_in_query;
use crate::notification_lambda_handler_run::{
    cleared_configuration, converted_document, converted_key, lambda_configuration, package,
    Emulator, BUDGET, CONVERTING_HANDLER, CSV_BODY, FUNCTIONS_URI, HANDLER, INVOKE_ACTION, LOOK,
    REGION, S3_PRINCIPAL,
};

/// The bucket, the function and the execution role this suite drives.
const BUCKET: &str = "notification-pipeline-dismantling";
const FUNCTION: &str = "dismantled-converter";
const ROLE: &str = "dismantled-converter-role";
const ROLE_POLICY: &str = "convert";

/// The event class a live configuration covers, and the one a narrowed configuration covers
/// instead: "s3:ObjectCreated:*" and "s3:ObjectRemoved:*" are both whole-type wildcards
/// (`smithy_query(query="shape",shape="Event")`), so a write is covered by exactly the first.
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";
const OBJECT_REMOVED: &str = "s3:ObjectRemoved:*";

/// How long a row that must see NOTHING keeps looking before concluding nothing came. Read only
/// after the row's own positive control has been converted, so the pipeline is known live and
/// known to have drained an event by then.
const QUIET_WINDOW: Duration = Duration::from_secs(4);

/// Which part of the pipeline a row takes away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TakenAway {
    /// The configuration is replaced by one naming the same function for ANOTHER event class, so
    /// nothing covers an object creation any more.
    EventClass,
    /// The configuration is cleared: "you can disable notifications by adding the empty
    /// NotificationConfiguration element" (`PutBucketNotificationConfiguration`'s own
    /// documentation).
    Configuration,
    /// The configuration is left exactly as recorded and the FUNCTION it names is deleted, so
    /// the destination is no longer there to take the publication.
    Destination,
}

/// One row: what it takes away, the key its positive control is written under, and the key whose
/// conversion must never appear afterwards.
struct Row {
    case: &'static str,
    taken_away: TakenAway,
    /// The key written through the LIVE pipeline, whose conversion is this row's barrier.
    live_key: &'static str,
    /// The key written after the row took its part of the pipeline away.
    probe_key: &'static str,
}

/// The rows, in the order they run: the destination is removed LAST, because a configuration
/// naming a function that no longer exists is one Amazon S3 would not record
/// ("Amazon S3 verifies that the Lambda function permissions grant Amazon S3 permission to
/// invoke the function", `PutBucketNotificationConfiguration`), so no later row could reinstall
/// a live pipeline.
const ROWS: [Row; 3] = [
    Row {
        case: "the configuration narrowed to another event class",
        taken_away: TakenAway::EventClass,
        live_key: "raw/live-class.csv",
        probe_key: "raw/probe-class.csv",
    },
    Row {
        case: "the configuration cleared with the empty container",
        taken_away: TakenAway::Configuration,
        live_key: "raw/live-cleared.csv",
        probe_key: "raw/probe-cleared.csv",
    },
    Row {
        case: "the destination function deleted",
        taken_away: TakenAway::Destination,
        live_key: "raw/live-destination.csv",
        probe_key: "raw/probe-destination.csv",
    },
];

/// Wait until the object at `key` is served, and answer what it held. A delivery is
/// asynchronous, so this waits for the CONDITION rather than for a span of this suite's own
/// choosing.
async fn converted_within(emulator: &Emulator, case: &str, key: &str) -> String {
    let deadline = Instant::now() + BUDGET;
    loop {
        let answer = emulator.s3("GET", &format!("/{BUCKET}/{key}"), &[]).await;
        if answer.status == 200 {
            return answer.body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the live pipeline never converted anything to {key:?} (last read \
             answered {} with {})",
            answer.status,
            answer.body
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Watch `key` for the whole of [`QUIET_WINDOW`], answering what the first read that served
/// something held — so a delivery made late still fails the row — or `None` when nothing was
/// ever served.
async fn appeared_within(emulator: &Emulator, key: &str) -> Option<String> {
    let until = Instant::now() + QUIET_WINDOW;
    loop {
        let answer = emulator.s3("GET", &format!("/{BUCKET}/{key}"), &[]).await;
        if (200..300).contains(&answer.status) {
            return Some(answer.body);
        }
        if Instant::now() >= until {
            return None;
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// The document `GetBucketNotificationConfiguration` reports for the bucket.
async fn reported_configuration(emulator: &Emulator, case: &str) -> String {
    let answer = emulator
        .s3("GET", &format!("/{BUCKET}?notification"), &[])
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: GetBucketNotificationConfiguration must succeed, got {} with {}",
        answer.status, answer.body
    );
    answer.body
}

/// The keys one page of `ListObjectsV2` reports, and the token it continues from — the listing a
/// client empties a bucket through.
async fn listed_page(
    emulator: &Emulator,
    case: &str,
    token: Option<&str>,
) -> (Vec<String>, Option<String>) {
    // Two per page, so the empty below has to FOLLOW the listing's own continuation rather than
    // assume one page holds everything.
    let mut uri = format!("/{BUCKET}?list-type=2&max-keys=2");
    if let Some(token) = token {
        uri.push_str("&continuation-token=");
        uri.push_str(&handed_back_in_query(token));
    }
    let answer = emulator.s3("GET", &uri, &[]).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: ListObjectsV2 must succeed, got {} with {}",
        answer.status, answer.body
    );
    let keys = elements_of(&answer.body, "Key");
    let next = elements_of(&answer.body, "NextContinuationToken")
        .into_iter()
        .next();
    (keys, next)
}

/// RULE: the delivery lives only while the configuration and its destination both do, and the
/// estate can then be removed with every read-back reporting the removal.
#[tokio::test]
async fn a_dismantled_notification_pipeline_publishes_nothing_and_tears_down_clean() {
    let emulator = Emulator::new();
    let account = engine::emulator_account_id();
    let function_arn = format!("arn:aws:lambda:{REGION}:{account}:function:{FUNCTION}");
    let live = lambda_configuration(&function_arn, OBJECT_CREATED);

    // ---- the estate: the execution role the handler's own Amazon S3 calls are made as, written
    // with IAM's own operations, the function, the grant Amazon S3 publishes under, and the
    // bucket.
    emulator
        .query(
            "setup",
            "iam",
            "CreateRole",
            &[
                ("RoleName", ROLE.to_string()),
                (
                    "AssumeRolePolicyDocument",
                    json!({
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    })
                    .to_string(),
                ),
            ],
        )
        .await;
    emulator
        .query(
            "setup",
            "iam",
            "PutRolePolicy",
            &[
                ("RoleName", ROLE.to_string()),
                ("PolicyName", ROLE_POLICY.to_string()),
                (
                    "PolicyDocument",
                    json!({
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Action": ["s3:GetObject", "s3:PutObject"],
                            "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
                        }],
                    })
                    .to_string(),
                ),
            ],
        )
        .await;
    let created = emulator
        .lambda(
            "POST",
            FUNCTIONS_URI,
            serde_json::to_vec(&json!({
                "FunctionName": FUNCTION,
                "Role": format!("arn:aws:iam::{account}:role/{ROLE}"),
                "Runtime": "python3.12",
                "Handler": HANDLER,
                "Timeout": 30,
                "Code": {
                    "ZipFile": base64::engine::general_purpose::STANDARD
                        .encode(package(CONVERTING_HANDLER)),
                },
            }))
            .expect("the create body serializes"),
        )
        .await;
    assert_eq!(
        created.status, 201,
        "case \"setup\": CreateFunction must succeed, got {} with {}",
        created.status, created.body
    );

    // An invocation needs a function in `Active`, which the clock owes: waited for by LOOKING
    // rather than by sleeping a span of this suite's own choosing.
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = emulator
            .lambda("GET", &format!("{FUNCTIONS_URI}/{FUNCTION}"), Vec::new())
            .await;
        assert_eq!(
            read.status, 200,
            "case \"setup\": GetFunction must succeed, got {} with {}",
            read.status, read.body
        );
        let reported: Value = serde_json::from_str(&read.body).unwrap_or(Value::Null);
        let state = reported["Configuration"]["State"].as_str().unwrap_or("");
        if state == "Active" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case \"setup\": the function never reached Active, last reported {state:?}"
        );
        tokio::time::sleep(LOOK).await;
    }

    let granted = emulator
        .lambda(
            "POST",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/policy"),
            serde_json::to_vec(&json!({
                "StatementId": "s3-invoke",
                "Action": INVOKE_ACTION,
                "Principal": S3_PRINCIPAL,
                "SourceArn": format!("arn:aws:s3:::{BUCKET}"),
            }))
            .expect("the permission body serializes"),
        )
        .await;
    assert_eq!(
        granted.status, 201,
        "case \"setup\": AddPermission must succeed, got {} with {}",
        granted.status, granted.body
    );
    emulator
        .s3_ok("setup", "PUT", &format!("/{BUCKET}"), &[])
        .await;

    // ---- the rows: each one makes the pipeline live, proves it live, takes its own part of the
    // pipeline away and then requires silence.
    let mut converted: Vec<String> = Vec::new();
    for row in &ROWS {
        let case = row.case;

        // Live again (and, for the first row, live for the first time): the whole container is
        // replaced, which is what this operation does.
        emulator
            .s3_ok(
                case,
                "PUT",
                &format!("/{BUCKET}?notification"),
                live.as_bytes(),
            )
            .await;
        emulator
            .s3_ok(
                case,
                "PUT",
                &format!("/{BUCKET}/{}", row.live_key),
                CSV_BODY.as_bytes(),
            )
            .await;
        let live_output = converted_key(row.live_key);
        let served = converted_within(&emulator, case, &live_output).await;
        let document: Value = serde_json::from_str(&served).unwrap_or_else(|error| {
            panic!("case {case:?}: expected the handler's converted document, got {error} for {served:?}")
        });
        assert_eq!(
            document,
            converted_document(),
            "case {case:?}: expected the live pipeline to convert {:?} row for row, got {document}",
            row.live_key
        );
        converted.push(live_output);

        // Take this row's part of the pipeline away.
        match row.taken_away {
            TakenAway::EventClass => {
                let narrowed = lambda_configuration(&function_arn, OBJECT_REMOVED);
                emulator
                    .s3_ok(
                        case,
                        "PUT",
                        &format!("/{BUCKET}?notification"),
                        narrowed.as_bytes(),
                    )
                    .await;
                let reported = reported_configuration(&emulator, case).await;
                assert!(
                    reported.contains(OBJECT_REMOVED) && !reported.contains(OBJECT_CREATED),
                    "case {case:?}: expected the narrowed configuration to be what is reported, \
                     got {reported}"
                );
            }
            TakenAway::Configuration => {
                emulator
                    .s3_ok(
                        case,
                        "PUT",
                        &format!("/{BUCKET}?notification"),
                        cleared_configuration().as_bytes(),
                    )
                    .await;
                let reported = reported_configuration(&emulator, case).await;
                assert!(
                    !reported.contains(&function_arn),
                    "case {case:?}: expected the cleared configuration to name no destination, \
                     got {reported}"
                );
            }
            TakenAway::Destination => {
                let removed = emulator
                    .lambda(
                        "DELETE",
                        &format!("{FUNCTIONS_URI}/{FUNCTION}"),
                        Vec::new(),
                    )
                    .await;
                assert_eq!(
                    removed.status, 204,
                    "case {case:?}: DeleteFunction must succeed, got {} with {}",
                    removed.status, removed.body
                );
                // The RECORDED configuration is untouched by its destination going away, which
                // is what lets a client tell a destination that is gone from a configuration
                // that was never written.
                let reported = reported_configuration(&emulator, case).await;
                assert!(
                    reported.contains(&function_arn) && reported.contains(OBJECT_CREATED),
                    "case {case:?}: expected the recorded configuration to survive the \
                     destination's removal, got {reported}"
                );
            }
        }

        // The write is accepted exactly as before: what a client is told about its own write
        // never depends on a delivery.
        let written = emulator
            .s3(
                "PUT",
                &format!("/{BUCKET}/{}", row.probe_key),
                CSV_BODY.as_bytes(),
            )
            .await;
        assert!(
            (200..300).contains(&written.status),
            "case {case:?}: the write of {:?} must be accepted whatever became of the pipeline, \
             got {} with {}",
            row.probe_key,
            written.status,
            written.body
        );
        let probe_output = converted_key(row.probe_key);
        let appeared = appeared_within(&emulator, &probe_output).await;
        assert!(
            appeared.is_none(),
            "case {case:?}: nothing covers the write of {:?} any more, so nothing may be written \
             to {probe_output:?}; the read served {:?}",
            row.probe_key,
            appeared
        );
        // And the objects the bucket already held are untouched by any of it.
        for key in &converted {
            let held = emulator.s3("HEAD", &format!("/{BUCKET}/{key}"), &[]).await;
            assert_eq!(
                held.status, 200,
                "case {case:?}: expected {key:?} to stay readable, got {} with {}",
                held.status, held.body
            );
        }
    }

    // ---- the teardown: the bucket emptied through its own paginated listing, then the bucket
    // and the role removed, each read-back reporting the removal.
    let case = "the estate's teardown";
    let deadline = Instant::now() + BUDGET;
    loop {
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let (page, next) = listed_page(&emulator, case, token.as_deref()).await;
            keys.extend(page);
            match next {
                Some(next) => token = Some(next),
                None => break,
            }
        }
        if keys.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the empty never converged; {BUCKET:?} still holds {keys:?}"
        );
        for key in keys {
            let dropped = emulator.s3("DELETE", &format!("/{BUCKET}/{key}"), &[]).await;
            assert_eq!(
                dropped.status, 204,
                "case {case:?}: DeleteObject({key:?}) must succeed, got {} with {}",
                dropped.status, dropped.body
            );
        }
    }
    let dropped = emulator.s3("DELETE", &format!("/{BUCKET}"), &[]).await;
    assert_eq!(
        dropped.status, 204,
        "case {case:?}: DeleteBucket must succeed on the emptied bucket, got {} with {}",
        dropped.status, dropped.body
    );
    let absent = emulator.s3("HEAD", &format!("/{BUCKET}"), &[]).await;
    assert_eq!(
        absent.status, 404,
        "case {case:?}: the deleted bucket must not resolve, got {} with {}",
        absent.status, absent.body
    );
    emulator
        .query(
            case,
            "iam",
            "DeleteRolePolicy",
            &[
                ("RoleName", ROLE.to_string()),
                ("PolicyName", ROLE_POLICY.to_string()),
            ],
        )
        .await;
    emulator
        .query(case, "iam", "DeleteRole", &[("RoleName", ROLE.to_string())])
        .await;
    let gone = emulator
        .send(
            "iam",
            "POST",
            "/",
            "application/x-www-form-urlencoded",
            format!("Action=GetRole&Version=2010-05-08&RoleName={ROLE}").into_bytes(),
        )
        .await;
    assert_eq!(
        gone.status, 404,
        "case {case:?}: the deleted role must not resolve, got {} with {}",
        gone.status, gone.body
    );
    assert!(
        elements_of(&gone.body, "Code").contains(&"NoSuchEntity".to_string()),
        "case {case:?}: expected the deleted role to be reported absent, got {}",
        gone.body
    );
}
