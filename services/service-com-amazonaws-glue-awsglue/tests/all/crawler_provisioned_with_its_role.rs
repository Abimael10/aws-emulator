// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for registering a crawler the way a client provisions one: an IAM role
//! the account has just been given, trusting the Glue service principal, and an ordinary
//! Amazon S3 target naming data that is not there yet.
//!
//! The rule under test: `CreateCrawler` judges the request it was SENT — the crawler's name,
//! the role it would run as, and that at least one target is named ("at least one crawl
//! target must be specified, in the s3Targets field, the jdbcTargets field, or the
//! DynamoDBTargets field") — and it decides nothing about the data another service holds
//! behind a target, which it merely REFERS to. So a crawler whose role IAM shows Glue can
//! assume and whose target names an Amazon S3 path is accepted with an empty body, and
//! `GetCrawler` then serves it in `READY` carrying exactly the definition that was sent,
//! whether or not the object store holds that bucket. The only refusals left are the modelled
//! ones about the request itself, whose wording this suite pins: `InvalidInputException` for a
//! `Targets` naming no store, and `EntityNotFoundException` for a crawler nothing created.
//!
//! WHY it matters: a client that provisions the role and the crawler together wraps the create
//! in a deadline-bounded retry over exactly `InvalidInputException` (the transient refusal real
//! Glue answers while a just-written trust document propagates to its assume probe), so a
//! create that keeps answering that code for a crawler the service should accept never
//! converges — the client spins until its own deadline instead of ever holding a crawler.
//!
//! The rows differ along what the rule generalises over: the form the `Role` member names the
//! role by, how deep the Amazon S3 path reaches into a bucket the store does not hold, and
//! whether the target's bucket exists at all. Amazon S3 is mounted so the store really answers
//! about those buckets, and IAM is mounted so the role resolution really has records to read.
//! Every call goes over the wire through the shared [`crate::wire`] harness.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::crawler_role_trust::{glue_trust, seed_role};
use crate::job_run_execution_role::emulator_with_iam_and_s3;
use crate::wire::{call, error_code, error_message, ok};

/// The role name the account holds, as a client's `CreateRole` would have written it.
const ROLE_NAME: &str = "sim2real-glue-crawler-service-role";

/// The database a crawler writes its tables into.
const DATABASE: &str = "crawler_provisioning_db";

/// A bucket the object store DOES hold, for the row that varies only in that.
const HELD_BUCKET: &str = "glue-crawler-provisioned-bucket";

/// A bucket no `CreateBucket` ever ran for: the data a crawler is registered ahead of.
const ABSENT_BUCKET: &str = "glue-crawler-unwritten-bucket";

/// `Crawler$State` of a crawler that is not crawling.
const READY: &str = "READY";

/// The whole `Message` a `Targets` naming no data store is refused with — written out here
/// rather than read from the crate's constant so this suite pins the wording.
const TARGETS_NOT_DEFINED: &str = "Targets are not defined";

/// One row: how the crawler names the role it runs as, and what its S3 target names.
struct Row {
    /// What every assertion of the row reports.
    case: &'static str,
    /// Whether the `Role` member carries the role's ARN rather than its bare name.
    role_by_arn: bool,
    /// The `S3Target$Path` the crawler is registered over.
    path: String,
}

/// The rows of the create table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded case: the role was just created, and the data is not written yet.
            case: "the role by ARN, over a prefix of a bucket the store does not hold",
            role_by_arn: true,
            path: format!("s3://{ABSENT_BUCKET}/data/"),
        },
        Row {
            case: "the role by ARN, over the root of a bucket the store does not hold",
            role_by_arn: true,
            path: format!("s3://{ABSENT_BUCKET}"),
        },
        Row {
            case: "the role by ARN, over a deep prefix of a bucket the store does not hold",
            role_by_arn: true,
            path: format!("s3://{ABSENT_BUCKET}/year=2026/month=01/day=02/"),
        },
        Row {
            case: "the role by its bare name, over a bucket the store does not hold",
            role_by_arn: false,
            path: format!("s3://{ABSENT_BUCKET}/data/"),
        },
        Row {
            case: "the role by ARN, over a prefix of a bucket the store DOES hold",
            role_by_arn: true,
            path: format!("s3://{HELD_BUCKET}/data/"),
        },
    ]
}

/// The `CreateCrawler` body one row sends.
fn crawler_body(name: &str, role: &str, path: &str) -> Value {
    json!({
        "Name": name,
        "Role": role,
        "DatabaseName": DATABASE,
        "Targets": { "S3Targets": [{ "Path": path }] },
    })
}

/// A crawler built on a freshly created Glue-assumable role is accepted over an ordinary
/// Amazon S3 target, whatever the object store holds behind it, and is then served in `READY`
/// carrying the definition that was sent.
#[tokio::test]
async fn a_crawler_on_a_glue_assumable_role_is_created_over_any_s3_target() {
    let (router, engine) = emulator_with_iam_and_s3();
    let role_arn = seed_role(&engine, ROLE_NAME, Some(glue_trust()));
    crate::wire::s3_client(&router)
        .create_bucket()
        .bucket(HELD_BUCKET)
        .send()
        .await
        .unwrap_or_else(|error| panic!("the held bucket must be created, got {error:?}"));
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let name = format!("provisioned-crawler-{index}");
        let role = if row.role_by_arn {
            role_arn.clone()
        } else {
            ROLE_NAME.to_string()
        };

        let (status, body) = call(
            &router,
            "CreateCrawler",
            &crawler_body(&name, &role, &row.path),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );
        assert_eq!(
            body,
            json!({}),
            "case {case:?}: CreateCrawler answers an empty body, got {body}"
        );

        let crawler = ok(&router, "GetCrawler", &json!({ "Name": name })).await["Crawler"].clone();
        assert_eq!(
            crawler["State"],
            json!(READY),
            "case {case:?}: a created crawler is READY, got {:?}",
            crawler["State"]
        );
        assert_eq!(
            crawler["Name"],
            json!(name),
            "case {case:?}: the crawler is served under the name it was created with, got {:?}",
            crawler["Name"]
        );
        assert_eq!(
            crawler["Role"],
            json!(role),
            "case {case:?}: the stored role must be the one sent, got {:?}",
            crawler["Role"]
        );
        assert_eq!(
            crawler["Targets"],
            json!({ "S3Targets": [{ "Path": row.path }] }),
            "case {case:?}: the stored targets must be the ones sent, got {:?}",
            crawler["Targets"]
        );
        assert_eq!(
            crawler["DatabaseName"],
            json!(DATABASE),
            "case {case:?}: the crawler writes into the database it was given, got {:?}",
            crawler["DatabaseName"]
        );
    }
}

/// The refusals that remain on the same role: a `Targets` naming no data store is the
/// modelled `InvalidInputException` with its own fixed sentence and stores nothing, and a
/// crawler nothing created is the modelled `EntityNotFoundException` naming the crawler the
/// request asked for.
///
/// The rows differ along the shape of the `Targets` that names nothing (absent lists, an empty
/// list, several empty lists at once) and along the name a read asks for.
#[tokio::test]
async fn the_remaining_refusals_are_about_the_request_itself() {
    let (router, engine) = emulator_with_iam_and_s3();
    let role_arn = seed_role(&engine, ROLE_NAME, Some(glue_trust()));
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;

    // (case, the `Targets` the create sends)
    let empty_targets: Vec<(&str, Value)> = vec![
        ("no target list at all", json!({})),
        ("an empty S3 target list", json!({ "S3Targets": [] })),
        (
            "an empty list of every kind at once",
            json!({
                "S3Targets": [],
                "JdbcTargets": [],
                "DynamoDBTargets": [],
                "CatalogTargets": [],
            }),
        ),
    ];

    for (index, (case, targets)) in empty_targets.into_iter().enumerate() {
        let name = format!("no-targets-crawler-{index}");
        let (status, body) = call(
            &router,
            "CreateCrawler",
            &json!({
                "Name": name,
                "Role": role_arn,
                "DatabaseName": DATABASE,
                "Targets": targets,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("InvalidInputException"),
            "case {case:?}: expected InvalidInputException, got {}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            TARGETS_NOT_DEFINED,
            "case {case:?}: expected the fixed no-targets sentence, got {:?}",
            error_message(&body)
        );

        // A refused create stored nothing, so the read of that very name is the absent-crawler
        // refusal naming it.
        assert_absent(&router, case, &name).await;
    }

    for case in [
        "a name nothing ever created",
        "Crawler-With-Mixed-Case",
        "a-name-carrying-dots.and-dashes",
    ] {
        assert_absent(&router, case, case).await;
    }
}

/// Assert `name` names no crawler: the read is the modelled `EntityNotFoundException` whose
/// sentence names the crawler the request asked for.
async fn assert_absent(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = call(router, "GetCrawler", &json!({ "Name": name })).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400 for an absent crawler, got {status} with {body}"
    );
    assert!(
        error_code(&body).ends_with("EntityNotFoundException"),
        "case {case:?}: expected EntityNotFoundException, got {}",
        error_code(&body)
    );
    assert_eq!(
        error_message(&body),
        format!("Crawler entry with name {name} does not exist"),
        "case {case:?}: the refusal must name the crawler asked for, got {:?}",
        error_message(&body)
    );
}
