// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a crawler's Amazon S3 targets are, to the write that registers
//! them: a REFERENCE to the data a later crawl would walk.
//!
//! The rule under test: `S3Target$Path` is "the path to the Amazon S3 target", which names
//! data held by ANOTHER service rather than a resource this create brings into being, and
//! `CreateCrawler`/`UpdateCrawler` model no not-found error for one — so the path is stored
//! exactly as the request wrote it and the object store is never asked whether it holds that
//! bucket. A crawler is routinely registered before the bucket or the data behind it exists,
//! so every one of these writes answers 200 and the crawler is readable, in `READY`, carrying
//! the targets it was given; whether a crawl can READ a target is decided by the crawl, on the
//! crawl (see [`crate::crawler_runs`]).
//!
//! The rows differ along what the targets name: a prefix holding data, a prefix holding
//! nothing, the bucket root, a bucket the store does not hold, a second target in such a
//! bucket beside a good one, a path that is no Amazon S3 address, and targets of another kind
//! altogether.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, and the buckets
//! and objects are staged through Amazon S3's own API — so what is asserted is HTTP behaviour
//! over an emulator whose object store really holds one of these buckets and not the other.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_script_execution::{create_bucket, emulator_with_s3, stage_script, BUCKET};
use crate::wire::{call, ok};

/// A bucket name no `CreateBucket` of this suite ever creates, so the store holds no such
/// bucket — the case the recorded client registers a crawler over.
const ABSENT_BUCKET: &str = "glue-crawler-target-absent";

/// The role every crawler here carries. No IAM is mounted in this suite, so nothing can be
/// shown about it and the role refuses no create — which is what leaves the TARGETS as the
/// one thing each row varies.
const ROLE: &str = "arn:aws:iam::000000000000:role/sim2real-crawler-role";

/// `Crawler$State` of a crawler that is not crawling, which every accepted create leaves
/// behind.
const READY: &str = "READY";

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

/// The stored crawler, read back through `GetCrawler`.
async fn reported_crawler(router: &MultiServiceRouter, name: &str) -> Value {
    ok(router, "GetCrawler", &json!({ "Name": name })).await["Crawler"].clone()
}

/// One row: what the crawler's targets name.
struct Row {
    /// What every assertion of the row reports.
    case: &'static str,
    /// The `Targets` the write sends, which a read must report back.
    targets: Value,
}

/// The rows, built once so the create suite and the update suite assert the same table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a prefix of a bucket the store holds, with data under it",
            targets: s3_targets(&[&format!("s3://{BUCKET}/data/")]),
        },
        Row {
            // A crawler is routinely registered before the data it crawls is written.
            case: "a prefix of a bucket the store holds that is empty",
            targets: s3_targets(&[&format!("s3://{BUCKET}/not-written-yet/")]),
        },
        Row {
            case: "the root of a bucket the store holds",
            targets: s3_targets(&[&format!("s3://{BUCKET}")]),
        },
        Row {
            // The recorded case: the bucket behind the target does not exist yet.
            case: "a bucket the store does not hold",
            targets: s3_targets(&[&format!("s3://{ABSENT_BUCKET}/data/")]),
        },
        Row {
            case: "a second target in a bucket the store does not hold",
            targets: s3_targets(&[
                &format!("s3://{BUCKET}/data/"),
                &format!("s3://{ABSENT_BUCKET}/other/"),
            ]),
        },
        Row {
            case: "a target under the s3a scheme, in a bucket the store does not hold",
            targets: s3_targets(&[&format!("s3a://{ABSENT_BUCKET}/data/")]),
        },
        Row {
            case: "a path that is no Amazon S3 address",
            targets: s3_targets(&["data/"]),
        },
        Row {
            case: "targets of another kind, which name no Amazon S3 path",
            targets: json!({ "DynamoDBTargets": [{ "Path": "crawler-target-table" }] }),
        },
    ]
}

/// `CreateCrawler` stores every Amazon S3 target it is given as written, whatever the object
/// store holds, and the crawler is readable in `READY` carrying those targets.
#[tokio::test]
async fn create_stores_every_s3_target_as_written() {
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
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );

        let crawler = reported_crawler(&router, &name).await;
        assert_eq!(
            crawler["Targets"], row.targets,
            "case {case:?}: the stored targets must be the ones sent, got {:?}",
            crawler["Targets"]
        );
        assert_eq!(
            crawler["State"],
            json!(READY),
            "case {case:?}: a created crawler is READY, got {:?}",
            crawler["State"]
        );
    }
}

/// `UpdateCrawler` replaces the targets the same way: the request's own targets are what the
/// crawler then carries, whatever the object store holds.
#[tokio::test]
async fn update_stores_the_targets_it_is_given() {
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
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );

        let crawler = reported_crawler(&router, &name).await;
        assert_eq!(
            crawler["Targets"], row.targets,
            "case {case:?}: the update's targets must be the stored ones, got {:?}",
            crawler["Targets"]
        );
    }
}
