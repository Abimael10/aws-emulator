// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the whole crawler-schedule surface of a crawler whose Amazon S3
//! target names a bucket THIS emulator's object store does not hold.
//!
//! The rule under test: `S3Target$Path` is "the path to the Amazon S3 target" — data another
//! service holds, which a LATER crawl walks — and `CreateCrawler`/`UpdateCrawler` state no
//! precondition that such a bucket be one this account's object store serves, so a crawler
//! write stores every target path as written and what the object store answers about a
//! target's bucket refuses nothing. The crawler is then readable in `READY`, and the schedule
//! toggles answer their own modelled refusals promptly: `NoScheduleException` for a crawler
//! carrying no cron expression on `StartCrawlerSchedule`, `SchedulerNotRunningException` for
//! the same crawler on `StopCrawlerSchedule`, and `EntityNotFoundException` for a
//! `CrawlerName` no crawler in this account has.
//!
//! The rows differ along what the targets name — the bucket of a path nothing created here,
//! the root of such a bucket, such a bucket under another URI scheme, a second target in one
//! beside a target in a bucket the store does hold, a path that is no Amazon S3 address, and
//! targets of another kind altogether — because the rule is about what the store holds, which
//! none of those forms changes.
//!
//! Glue, IAM and Amazon S3 are mounted on ONE engine, the way `core/router/src/lib.rs` mounts
//! them, and the role every crawler runs as is created through IAM's own API — so the create
//! each row makes is the create a client makes against the assembled emulator, with an object
//! store there to answer and a role IAM really holds. The same rule over `UpdateCrawler`, and
//! over a bucket the store refuses to let this caller read, is driven by
//! [`crate::crawler_s3_target_resolution`].

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::crawler_s3_target_resolution::{as_reported, s3_targets};
use crate::job_run_event_target_permission::iam;
use crate::wire::{call, emulator_on, error_code, error_message, ok, s3_client};

/// The role every crawler here runs as, created through IAM's own `CreateRole`.
const ROLE_NAME: &str = "glue-crawler-schedule-role";

/// A bucket this suite DOES create, so the object store serves it: the control row, which is
/// what makes the other rows about the store's answer rather than about the create refusing
/// nothing at all.
const HELD_BUCKET: &str = "glue-crawler-schedule-held";

/// A bucket no call of this suite ever creates, so the object store answers that it holds no
/// bucket under that name — the case a client deploys a crawler over when the data is written
/// later, or lives in another account.
const UNHELD_BUCKET: &str = "glue-crawler-schedule-unheld";

/// `Crawler$State` of a crawler with no crawl under way, which every accepted create leaves
/// behind.
const READY: &str = "READY";

/// `Schedule$State` of a crawler whose schedule is in force, and of one whose is not.
const SCHEDULED: &str = "SCHEDULED";
/// See [`SCHEDULED`].
const NOT_SCHEDULED: &str = "NOT_SCHEDULED";

/// A cron expression of the documented form, from `UpdateCrawlerSchedule$Schedule` ("to run
/// something every day at 12:15 UTC, you would specify: cron(15 12 * * ? *)").
const CRON: &str = "cron(15 12 * * ? *)";

/// An emulator with Glue, IAM and Amazon S3 on one shared engine, holding [`HELD_BUCKET`] and
/// a role IAM reports Glue can assume — answering the role's ARN.
async fn emulator_with_a_role_and_a_bucket() -> (MultiServiceRouter, String) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = emulator_on(Arc::clone(&engine));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));

    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "glue.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        &router,
        "CreateRole",
        &format!("RoleName={ROLE_NAME}&AssumeRolePolicyDocument={trust}"),
    )
    .await;
    let role = created
        .split("<Arn>")
        .nth(1)
        .and_then(|rest| rest.split("</Arn>").next())
        .unwrap_or_else(|| panic!("CreateRole must answer the role's Arn, got {created}"))
        .to_string();

    s3_client(&router)
        .create_bucket()
        .bucket(HELD_BUCKET)
        .send()
        .await
        .unwrap_or_else(|error| panic!("the bucket {HELD_BUCKET} must be created, got {error:?}"));

    (router, role)
}

/// The `CreateCrawler` body of a crawler named `name` over `targets`, carrying NO `Schedule`
/// — which is what leaves it the crawler the schedule toggles have nothing to act on.
fn crawler_body(name: &str, role: &str, targets: &Value) -> Value {
    json!({
        "Name": name,
        "Role": role,
        "DatabaseName": "crawler_schedule_db",
        "Targets": targets.clone(),
    })
}

/// The `Crawler` structure `GetCrawler` reports for `name`.
async fn reported_crawler(router: &MultiServiceRouter, name: &str) -> Value {
    ok(router, "GetCrawler", &json!({ "Name": name })).await["Crawler"].clone()
}

/// Assert that `body` is the refusal `operation` answered, carrying `code` and exactly
/// `message`.
fn assert_refusal(case: &str, status: u16, body: &Value, code: &str, message: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400, got {status} with {body}"
    );
    assert!(
        error_code(body).contains(code),
        "case {case:?}: expected {code}, got {:?} with {body}",
        error_code(body)
    );
    assert_eq!(
        error_message(body),
        message,
        "case {case:?}: expected Message {message:?}, got {:?}",
        error_message(body)
    );
}

/// What one row's crawler crawls, and the case it reports.
fn rows() -> Vec<(&'static str, Value)> {
    let held = format!("s3://{HELD_BUCKET}/data/");
    vec![
        (
            "a prefix of a bucket the object store holds",
            s3_targets(&[&held]),
        ),
        (
            "a prefix of a bucket the object store holds nothing under",
            s3_targets(&[&format!("s3://{UNHELD_BUCKET}/data/")]),
        ),
        (
            "the root of a bucket the object store holds nothing under",
            s3_targets(&[&format!("s3://{UNHELD_BUCKET}")]),
        ),
        (
            "such a bucket named under the s3a scheme",
            s3_targets(&[&format!("s3a://{UNHELD_BUCKET}/data/")]),
        ),
        (
            "a target in such a bucket beside one the store holds",
            s3_targets(&[&held, &format!("s3://{UNHELD_BUCKET}/other/")]),
        ),
        (
            "the same unheld bucket named twice",
            s3_targets(&[
                &format!("s3://{UNHELD_BUCKET}/first/"),
                &format!("s3://{UNHELD_BUCKET}/second/"),
            ]),
        ),
        (
            "a path that is no Amazon S3 address",
            s3_targets(&["data/"]),
        ),
        (
            "targets of another kind, which name no Amazon S3 path",
            json!({ "DynamoDBTargets": [{ "Path": "crawler-schedule-table" }] }),
        ),
    ]
}

/// Every row of the table is created and readable in `READY` carrying the targets it was sent,
/// whatever the object store answers about the buckets they name.
#[tokio::test]
async fn a_crawler_is_created_over_a_target_the_object_store_does_not_hold() {
    let (router, role) = emulator_with_a_role_and_a_bucket().await;

    for (index, (case, targets)) in rows().into_iter().enumerate() {
        let name = format!("schedule-target-{index}");
        let (status, body) = call(
            &router,
            "CreateCrawler",
            &crawler_body(&name, &role, &targets),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be admitted, got {status} with {body}"
        );

        let crawler = reported_crawler(&router, &name).await;
        assert_eq!(
            crawler["Targets"],
            as_reported(&targets),
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

/// The schedule surface of such a crawler answers its modelled refusals: a crawler carrying no
/// cron expression has no schedule to start and no scheduler to stop, and a name no crawler has
/// is the schedule surface's own miss — for every one of the three operations.
#[tokio::test]
async fn the_schedule_toggles_refuse_a_crawler_with_no_schedule_and_an_unknown_crawler() {
    let (router, role) = emulator_with_a_role_and_a_bucket().await;
    let unheld = format!("s3://{UNHELD_BUCKET}/data/");

    for (index, (case, targets)) in rows().into_iter().enumerate() {
        let name = format!("schedule-refusal-{index}");
        ok(
            &router,
            "CreateCrawler",
            &crawler_body(&name, &role, &targets),
        )
        .await;

        // A crawler carrying no cron expression: the start has no schedule to put in force.
        let (status, body) = call(
            &router,
            "StartCrawlerSchedule",
            &json!({ "CrawlerName": name }),
        )
        .await;
        assert_refusal(
            case,
            status,
            &body,
            "NoScheduleException",
            "No schedule found.",
        );

        // ... and the stop has no scheduler to suspend.
        let (status, body) = call(
            &router,
            "StopCrawlerSchedule",
            &json!({ "CrawlerName": name }),
        )
        .await;
        assert_refusal(
            case,
            status,
            &body,
            "SchedulerNotRunningException",
            &format!("Scheduler for crawler with name {name} is not running"),
        );

        // The crawler is untouched by either refusal: it is still there, still unscheduled.
        let crawler = reported_crawler(&router, &name).await;
        assert_eq!(
            crawler["Schedule"],
            Value::Null,
            "case {case:?}: a refused toggle must leave the crawler unscheduled, got {:?}",
            crawler["Schedule"]
        );
    }

    // A `CrawlerName` no crawler in this account has, on each of the three operations.
    let absent = "schedule-no-such-crawler";
    for (operation, code, message) in [
        (
            "StartCrawlerSchedule",
            "EntityNotFoundException",
            format!("Crawler with name {absent} does not exist not found."),
        ),
        (
            "StopCrawlerSchedule",
            "EntityNotFoundException",
            format!("Crawler with name {absent} does not exist not found."),
        ),
        (
            "UpdateCrawlerSchedule",
            "EntityNotFoundException",
            format!("Crawler entry with name {absent} does not exist"),
        ),
    ] {
        let mut request = json!({ "CrawlerName": absent });
        if operation == "UpdateCrawlerSchedule" {
            request["Schedule"] = json!(CRON);
        }
        let (status, body) = call(&router, operation, &request).await;
        assert_refusal(operation, status, &body, code, &message);
    }

    // And the state machine itself still walks, on a crawler over the unheld bucket: the write
    // of a cron expression puts a schedule in force, the stop suspends it, the start resumes it.
    let walked = "schedule-walk";
    ok(
        &router,
        "CreateCrawler",
        &crawler_body(walked, &role, &s3_targets(&[&unheld])),
    )
    .await;
    ok(
        &router,
        "UpdateCrawlerSchedule",
        &json!({ "CrawlerName": walked, "Schedule": CRON }),
    )
    .await;
    let crawler = reported_crawler(&router, walked).await;
    assert_eq!(
        crawler["Schedule"]["ScheduleExpression"],
        json!(CRON),
        "the written cron expression must be the crawler's schedule, got {crawler}"
    );
    assert_eq!(
        crawler["Schedule"]["State"],
        json!(SCHEDULED),
        "a crawler carrying a cron expression is SCHEDULED, got {crawler}"
    );

    ok(
        &router,
        "StopCrawlerSchedule",
        &json!({ "CrawlerName": walked }),
    )
    .await;
    assert_eq!(
        reported_crawler(&router, walked).await["Schedule"]["State"],
        json!(NOT_SCHEDULED),
        "a suspended schedule is NOT_SCHEDULED"
    );

    ok(
        &router,
        "StartCrawlerSchedule",
        &json!({ "CrawlerName": walked }),
    )
    .await;
    assert_eq!(
        reported_crawler(&router, walked).await["Schedule"]["State"],
        json!(SCHEDULED),
        "a resumed schedule is SCHEDULED again"
    );
}
