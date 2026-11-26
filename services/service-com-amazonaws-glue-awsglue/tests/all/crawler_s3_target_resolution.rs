// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how a crawler write resolves the BUCKET behind each of its Amazon S3
//! targets.
//!
//! The rule under test: `S3Target$Path` is "the path to the Amazon S3 target" — data held by
//! another service, which a crawler definition REFERS to — so a crawler write refuses a target
//! only when the object store ANSWERS that it holds no bucket under that target's bucket name;
//! a bucket it serves, a bucket it refuses to let this caller read, and an emulator with no
//! object store to ask all establish no such thing and are stored as written. The refusal is the
//! operation's own modelled `InvalidInputException` (HTTP 400) and it names the target and the
//! 404 Amazon S3 answered; nothing is persisted by it.
//!
//! The rows differ along what the targets name: a prefix holding data, a prefix holding
//! nothing, the bucket root, a bucket the store holds nothing under, a second target in such a
//! bucket beside a good one, another URI scheme over such a bucket, the same absent bucket
//! twice, a path that is no Amazon S3 address, and targets of another kind altogether.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, and the buckets,
//! objects and bucket policies are staged through Amazon S3's own API — so what is asserted is
//! HTTP behaviour over an emulator whose object store really holds one of these buckets and not
//! the other.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_script_execution::{create_bucket, emulator_with_s3, stage_script, BUCKET};
use crate::wire::{call, emulator, error_code, error_message, ok, s3_client};

/// A bucket name no `CreateBucket` of this suite ever creates, so the object store answers that
/// it holds no such bucket — the case the recorded client deploys a crawler over.
const ABSENT_BUCKET: &str = "glue-crawler-target-absent";

/// A bucket this suite DOES create and then denies every listing of, so a read of it is
/// refused rather than answered absent.
const DENIED_BUCKET: &str = "glue-crawler-target-denied";

/// The role every crawler here carries. No IAM is mounted in this suite, so nothing can be
/// shown about it and the role refuses no create — which is what leaves the TARGETS as the
/// one thing each row varies.
const ROLE: &str = "arn:aws:iam::000000000000:role/sim2real-crawler-role";

/// `Crawler$State` of a crawler that is not crawling, which every accepted create leaves
/// behind.
const READY: &str = "READY";

/// The error code a refused crawler write answers with: `CreateCrawler` models
/// `InvalidInputException` as its client-input refusal.
const INVALID_INPUT: &str = "InvalidInputException";

/// The error code a read of a crawler no write created answers with.
const ENTITY_NOT_FOUND: &str = "EntityNotFoundException";

/// The `CreateCrawler` body of a crawler named `name` over `targets`.
fn crawler_body(name: &str, targets: Value) -> Value {
    json!({
        "Name": name,
        "Role": ROLE,
        "DatabaseName": "crawler_target_db",
        "Targets": targets,
    })
}

/// A `CrawlerTargets` naming each of `paths` as an Amazon S3 target.
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

/// Every target list a crawler reports: the eight the published `CrawlerTargets` binds
/// (`smithy_query shape=CrawlerTargets`) and the REST catalog list the service reports beside
/// them.
const REPORTED_TARGET_LISTS: [&str; 9] = [
    "S3Targets",
    "JdbcTargets",
    "MongoDBTargets",
    "DynamoDBTargets",
    "CatalogTargets",
    "DeltaTargets",
    "IcebergTargets",
    "HudiTargets",
    "RestCatalogTargets",
];

/// The `Targets` a read reports for a write that sent `sent`: the lists it named, every list
/// it did not name EMPTY, and each Amazon S3 target carrying the glob patterns it excludes —
/// none, which is the empty list.
fn as_reported(sent: &Value) -> Value {
    let mut reported = json!({});
    for list in REPORTED_TARGET_LISTS {
        reported[list] = sent.get(list).cloned().unwrap_or_else(|| json!([]));
    }
    for target in reported["S3Targets"].as_array_mut().into_iter().flatten() {
        if target.get("Exclusions").is_none() {
            target["Exclusions"] = json!([]);
        }
    }
    reported
}

/// One row: what the crawler's targets name, and the target a write of them must be refused
/// for — `None` for a row every write stores as written.
struct Row {
    /// What every assertion of the row reports.
    case: &'static str,
    /// The `Targets` the write sends.
    targets: Value,
    /// The `Path`, exactly as the row wrote it, that the refusal must name — or `None` when
    /// the write is admitted.
    refused_for: Option<String>,
}

/// The rows, built once so the create suite and the update suite assert the same table.
fn rows() -> Vec<Row> {
    let held = format!("s3://{BUCKET}/data/");
    let absent = format!("s3://{ABSENT_BUCKET}/data/");
    vec![
        Row {
            case: "a prefix of a bucket the store holds, with data under it",
            targets: s3_targets(&[&held]),
            refused_for: None,
        },
        Row {
            // A crawler is routinely registered before the data it crawls is written.
            case: "a prefix of a bucket the store holds that is empty",
            targets: s3_targets(&[&format!("s3://{BUCKET}/not-written-yet/")]),
            refused_for: None,
        },
        Row {
            case: "the root of a bucket the store holds",
            targets: s3_targets(&[&format!("s3://{BUCKET}")]),
            refused_for: None,
        },
        Row {
            // The recorded case: the deployment names a bucket nothing holds.
            case: "a bucket the store holds nothing under",
            targets: s3_targets(&[&absent]),
            refused_for: Some(absent.clone()),
        },
        Row {
            case: "a second target in a bucket the store holds nothing under",
            targets: s3_targets(&[&held, &format!("s3://{ABSENT_BUCKET}/other/")]),
            refused_for: Some(format!("s3://{ABSENT_BUCKET}/other/")),
        },
        Row {
            case: "a target under the s3a scheme, in a bucket the store holds nothing under",
            targets: s3_targets(&[&format!("s3a://{ABSENT_BUCKET}/data/")]),
            refused_for: Some(format!("s3a://{ABSENT_BUCKET}/data/")),
        },
        Row {
            case: "the same absent bucket named twice, which is one bucket to ask about",
            targets: s3_targets(&[&absent, &format!("s3://{ABSENT_BUCKET}/second/")]),
            refused_for: Some(absent.clone()),
        },
        Row {
            case: "a path that is no Amazon S3 address, which names no bucket to ask about",
            targets: s3_targets(&["data/"]),
            refused_for: None,
        },
        Row {
            case: "targets of another kind, which name no Amazon S3 path",
            targets: json!({ "DynamoDBTargets": [{ "Path": "crawler-target-table" }] }),
            refused_for: None,
        },
    ]
}

/// Stage the emulator every row of the table runs against: Glue beside Amazon S3 on one
/// engine, holding the suite's bucket with one object under `data/`.
async fn emulator_holding_the_bucket() -> MultiServiceRouter {
    let (router, _engine) = emulator_with_s3();
    create_bucket(&router).await;
    stage_script(&router, "data/input.csv", "id,name\n1,ada\n").await;
    router
}

/// Assert that `body` is the refusal a crawler write answers an absent Amazon S3 target with:
/// the modelled `InvalidInputException` whose message names the target `path` and the 404
/// Amazon S3 answered about its bucket.
fn assert_absent_target_refusal(case: &str, status: u16, body: &Value, path: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: an absent S3 target is refused with 400, got {status} with {body}"
    );
    let code = error_code(body);
    assert!(
        code.contains(INVALID_INPUT),
        "case {case:?}: expected {INVALID_INPUT}, got {code:?} with {body}"
    );
    let message = error_message(body);
    for expected in ["S3 target", path, "404", "NoSuchBucket", "Amazon S3"] {
        assert!(
            message.contains(expected),
            "case {case:?}: the refusal must state {expected:?}, got {message:?}"
        );
    }
}

/// `CreateCrawler` resolves the bucket behind every Amazon S3 target against the object store:
/// a target whose bucket the store answers it holds nothing under is refused, naming that
/// target and the 404, and nothing is persisted; every other row is stored as written and the
/// crawler is readable in `READY` carrying the targets it was given.
#[tokio::test]
async fn create_resolves_every_s3_target_against_the_object_store() {
    let router = emulator_holding_the_bucket().await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let name = format!("target-create-{index}");
        let (status, body) = call(
            &router,
            "CreateCrawler",
            &crawler_body(&name, row.targets.clone()),
        )
        .await;

        let Some(path) = row.refused_for else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} with {body}"
            );
            let crawler = reported_crawler(&router, &name).await;
            assert_eq!(
                crawler["Targets"],
                as_reported(&row.targets),
                "case {case:?}: the stored targets must be the ones sent, got {:?}",
                crawler["Targets"]
            );
            assert_eq!(
                crawler["State"],
                json!(READY),
                "case {case:?}: a created crawler is READY, got {:?}",
                crawler["State"]
            );
            continue;
        };

        assert_absent_target_refusal(case, status, &body, &path);
        // A refused create persists nothing: the crawler it would have written is not there.
        let (read_status, read_body) = call(&router, "GetCrawler", &json!({ "Name": name })).await;
        assert_eq!(
            read_status, 400,
            "case {case:?}: a refused create leaves no crawler to read, got {read_status} with \
             {read_body}"
        );
        let code = error_code(&read_body);
        assert!(
            code.contains(ENTITY_NOT_FOUND),
            "case {case:?}: expected {ENTITY_NOT_FOUND} for the refused crawler, got {code:?}"
        );
    }
}

/// `UpdateCrawler` resolves the targets it would leave behind the same way: a replacement whose
/// bucket the store answers it holds nothing under is refused and the stored targets are left
/// as they were, while every other row becomes the crawler's targets.
#[tokio::test]
async fn update_resolves_the_targets_it_would_store() {
    let router = emulator_holding_the_bucket().await;
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

        let Some(path) = row.refused_for else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} with {body}"
            );
            let crawler = reported_crawler(&router, &name).await;
            assert_eq!(
                crawler["Targets"],
                as_reported(&row.targets),
                "case {case:?}: the update's targets must be the stored ones, got {:?}",
                crawler["Targets"]
            );
            continue;
        };

        assert_absent_target_refusal(case, status, &body, &path);
        let crawler = reported_crawler(&router, &name).await;
        assert_eq!(
            crawler["Targets"],
            as_reported(&s3_targets(&[&held])),
            "case {case:?}: a refused update leaves the stored targets as they were, got {:?}",
            crawler["Targets"]
        );
    }
}

/// A bucket the object store HOLDS but refuses to let this caller read is not a bucket that is
/// absent: the read answers 403 rather than 404, so the create is admitted.
#[tokio::test]
async fn a_bucket_the_object_store_refuses_to_read_is_not_absent() {
    let router = emulator_holding_the_bucket().await;
    let s3 = s3_client(&router);
    s3.create_bucket()
        .bucket(DENIED_BUCKET)
        .send()
        .await
        .unwrap_or_else(|error| panic!("the bucket {DENIED_BUCKET} must be created, got {error:?}"));
    s3.put_bucket_policy()
        .bucket(DENIED_BUCKET)
        .policy(
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": "*",
                    "Action": "s3:ListBucket",
                    "Resource": format!("arn:aws:s3:::{DENIED_BUCKET}"),
                }],
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap_or_else(|error| panic!("the deny policy must be stored, got {error:?}"));

    // The premise of the test: the object store answers the read of this bucket with a
    // refusal, not with "no such bucket".
    let head = s3.head_bucket().bucket(DENIED_BUCKET).send().await;
    let status = head
        .as_ref()
        .err()
        .and_then(|error| error.raw_response().map(|raw| raw.status().as_u16()));
    assert_eq!(
        status,
        Some(403),
        "the object store must refuse the read of {DENIED_BUCKET} with 403, got {head:?}"
    );

    let path = format!("s3://{DENIED_BUCKET}/data/");
    let (create_status, body) = call(
        &router,
        "CreateCrawler",
        &crawler_body("target-denied", s3_targets(&[&path])),
    )
    .await;
    assert_eq!(
        create_status, 200,
        "a bucket whose read is refused is not absent, got {create_status} with {body}"
    );
    let crawler = reported_crawler(&router, "target-denied").await;
    assert_eq!(
        crawler["Targets"],
        as_reported(&s3_targets(&[&path])),
        "the stored targets must be the ones sent, got {:?}",
        crawler["Targets"]
    );
}

/// With NO object store mounted to ask, nothing is established about any target, so every row
/// of the table — the absent ones included — is stored as written.
#[tokio::test]
async fn with_no_object_store_to_ask_every_target_is_stored() {
    let router = emulator();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let name = format!("target-unanswered-{index}");
        let (status, body) = call(
            &router,
            "CreateCrawler",
            &crawler_body(&name, row.targets.clone()),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: with no object store to ask the create is admitted, got {status} \
             with {body}"
        );
        let crawler = reported_crawler(&router, &name).await;
        assert_eq!(
            crawler["Targets"],
            as_reported(&row.targets),
            "case {case:?}: the stored targets must be the ones sent, got {:?}",
            crawler["Targets"]
        );
    }
}
