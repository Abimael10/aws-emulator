// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the RESOLUTION of a crawler's Amazon S3 targets against the object
//! store, on the create and on the update.
//!
//! The rule under test: `S3Target$Path` is "the path to the Amazon S3 target", and a crawler
//! is registered against the targets it will crawl — so each of those paths is resolved
//! against the object store when the crawler is written, and a path whose BUCKET the store
//! does not hold is refused with the operation's modelled `InvalidInputException`, naming that
//! target and the store's own answer about it, with nothing written. Only what the store
//! ANSWERED refuses the write: a bucket it serves is accepted whether or not anything is under
//! the path yet — a crawler is routinely registered before the data it crawls is written — and
//! a path the resolution cannot put to the store refuses nothing.
//!
//! The rows differ along what the targets name: a prefix holding data, a prefix holding
//! nothing, the bucket root, a bucket the store does not hold, a second target in such a
//! bucket beside a good one, a path that is no Amazon S3 address, and targets of another kind
//! altogether.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, and the buckets
//! and objects are staged through Amazon S3's own API — so what is asserted is HTTP behaviour
//! over state the object store really holds.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_script_execution::{create_bucket, emulator_with_s3, stage_script, BUCKET};
use crate::wire::{call, error_code, error_message, ok};

/// A bucket name no `CreateBucket` of this suite ever creates, so the store answers that it
/// holds no such bucket.
const ABSENT_BUCKET: &str = "glue-crawler-target-absent";

/// The role every crawler here carries. No IAM is mounted in this suite, so nothing can be
/// shown about it and the role refuses no create — which is what leaves the TARGETS as the
/// one thing each row varies.
const ROLE: &str = "arn:aws:iam::000000000000:role/sim2real-crawler-role";

/// The fixed words the refusal of a target the store does not hold carries, written out here
/// rather than read from the crate's constant so this suite pins the wording.
const S3_TARGET: &str = "S3 target";

/// The status the object store's own answer about a bucket it does not hold carries, which the
/// refusal states.
const NOT_FOUND_STATUS: &str = "404";

/// The `CreateCrawler` body of a crawler named `name` over `targets`.
fn crawler_body(name: &str, targets: Value) -> Value {
    json!({
        "Name": name,
        "Role": ROLE,
        "DatabaseName": "crawler_target_db",
        "Targets": targets,
    })
}

/// A `CrawlerTargets` naming one Amazon S3 path.
fn s3_targets(paths: &[&str]) -> Value {
    json!({
        "S3Targets": paths
            .iter()
            .map(|path| json!({ "Path": path }))
            .collect::<Vec<Value>>(),
    })
}

/// Whether the store holds a crawler of this name, read through `GetCrawler`.
async fn crawler_is_held(router: &MultiServiceRouter, name: &str) -> bool {
    let (status, _body) = call(router, "GetCrawler", &json!({ "Name": name })).await;
    status == 200
}

/// One row: what the crawler's targets name, and whether the write is refused for them.
struct Row {
    /// What every assertion of the row reports.
    case: &'static str,
    /// The `Targets` the write sends.
    targets: Value,
    /// The target path the refusal must name, or `None` for a row that is not refused.
    refused_path: Option<String>,
}

/// The rows, built once so the create suite and the update suite assert the same table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a prefix of a bucket the store holds, with data under it",
            targets: s3_targets(&[&format!("s3://{BUCKET}/data/")]),
            refused_path: None,
        },
        Row {
            // A crawler is routinely registered before the data it crawls is written.
            case: "a prefix of a bucket the store holds that is empty",
            targets: s3_targets(&[&format!("s3://{BUCKET}/not-written-yet/")]),
            refused_path: None,
        },
        Row {
            case: "the root of a bucket the store holds",
            targets: s3_targets(&[&format!("s3://{BUCKET}")]),
            refused_path: None,
        },
        Row {
            case: "a bucket the store does not hold",
            targets: s3_targets(&[&format!("s3://{ABSENT_BUCKET}/data/")]),
            refused_path: Some(format!("s3://{ABSENT_BUCKET}/data/")),
        },
        Row {
            case: "a second target in a bucket the store does not hold",
            targets: s3_targets(&[
                &format!("s3://{BUCKET}/data/"),
                &format!("s3://{ABSENT_BUCKET}/other/"),
            ]),
            refused_path: Some(format!("s3://{ABSENT_BUCKET}/other/")),
        },
        Row {
            case: "a target under the s3a scheme, in a bucket the store does not hold",
            targets: s3_targets(&[&format!("s3a://{ABSENT_BUCKET}/data/")]),
            refused_path: Some(format!("s3a://{ABSENT_BUCKET}/data/")),
        },
        Row {
            // The resolution cannot put this path to the store, so it says nothing about it.
            case: "a path that is no Amazon S3 address",
            targets: s3_targets(&["data/"]),
            refused_path: None,
        },
        Row {
            case: "targets of another kind, which name no Amazon S3 path",
            targets: json!({ "DynamoDBTargets": [{ "Path": "crawler-target-table" }] }),
            refused_path: None,
        },
    ]
}

/// Assert `body` is the refusal a target the store does not hold earns: the modelled
/// `InvalidInputException` naming that target and the store's own 404.
fn assert_target_refusal(case: &str, status: u16, body: &Value, path: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400 for {path:?}, got {status} with {body}"
    );
    assert!(
        error_code(body).ends_with("InvalidInputException"),
        "case {case:?}: expected InvalidInputException, got {}",
        error_code(body)
    );
    let message = error_message(body);
    for held in [S3_TARGET, path, NOT_FOUND_STATUS] {
        assert!(
            message.contains(held),
            "case {case:?}: expected the message to carry {held:?}, got {message:?}"
        );
    }
}

/// `CreateCrawler` resolves every Amazon S3 target it is given, and stores no crawler whose
/// target names a bucket the object store does not hold.
#[tokio::test]
async fn create_resolves_every_s3_target_against_the_object_store() {
    let (router, _engine) = emulator_with_s3();
    create_bucket(&router).await;
    stage_script(&router, "data/input.csv", "id,name\n1,ada\n").await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let name = format!("target-create-{index}");
        let (status, body) = call(
            &router,
            "CreateCrawler",
            &crawler_body(&name, row.targets.clone()),
        )
        .await;
        match &row.refused_path {
            Some(path) => {
                assert_target_refusal(case, status, &body, path);
                assert!(
                    !crawler_is_held(&router, &name).await,
                    "case {case:?}: a refused create stored no crawler"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected 200, got {status} with {body}"
                );
                assert!(
                    crawler_is_held(&router, &name).await,
                    "case {case:?}: the created crawler must be readable"
                );
            }
        }
    }
}

/// `UpdateCrawler` resolves the targets it would leave behind the same way, and writes nothing
/// for a target the object store does not hold: the crawler keeps the targets it was created
/// with.
#[tokio::test]
async fn update_resolves_the_targets_it_would_leave_behind() {
    let (router, _engine) = emulator_with_s3();
    create_bucket(&router).await;
    stage_script(&router, "data/input.csv", "id,name\n1,ada\n").await;
    let held = format!("s3://{BUCKET}/data/");

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let name = format!("target-update-{index}");
        ok(
            &router,
            "CreateCrawler",
            &crawler_body(&name, s3_targets(&[&held])),
        )
        .await;

        let (status, body) = call(
            &router,
            "UpdateCrawler",
            &json!({ "Name": name, "Targets": row.targets.clone() }),
        )
        .await;
        let reported = ok(&router, "GetCrawler", &json!({ "Name": name })).await;
        let targets = reported["Crawler"]["Targets"].clone();
        match &row.refused_path {
            Some(path) => {
                assert_target_refusal(case, status, &body, path);
                assert_eq!(
                    targets,
                    s3_targets(&[&held]),
                    "case {case:?}: a refused update left the stored targets alone"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected 200, got {status} with {body}"
                );
                assert_eq!(
                    targets, row.targets,
                    "case {case:?}: the update's targets must be the stored ones"
                );
            }
        }
    }
}
