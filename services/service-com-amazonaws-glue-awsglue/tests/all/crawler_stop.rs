// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `StopCrawler`: what stopping a crawl does to the crawler, and what
//! the call answers for a crawler with no crawl to stop.
//!
//! The rule under test: "if the specified crawler is running, stops the crawl" — so a crawler
//! whose crawl is under way leaves `RUNNING` for `STOPPING` with an empty body, the clock owes
//! it its way back to `READY` one span later, and the crawl it stopped is then reported with
//! the modelled `LastCrawlStatus` `CANCELLED` (sources: `StopCrawler`'s own documentation,
//! `Crawler$State` "indicates whether the crawler is running, or whether a run is pending",
//! and `LastCrawlInfo$Status`). Every other case is one of the three classes the operation
//! models: a crawler with no crawl under way has none to stop
//! (`CrawlerNotRunningException`), a crawl that was already asked to stop leaves nothing to
//! ask for (`CrawlerStoppingException`), and a name no crawler in this account has is
//! `EntityNotFoundException`.
//!
//! The due instant the owed finish falls at is moved THROUGH THE STORE rather than slept for
//! ([`crate::crawler_runs::set_due_instant`]), so every transition here is deterministic and
//! nothing waits on the wall clock. Calls go over the wire through the shared [`crate::wire`]
//! harness, and the objects the crawl reads are staged through Amazon S3's own `PutObject`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::crawler_runs::{
    already_past, far_future, given_crawler_over, given_database, set_due_instant, stage, DATABASE,
};
use crate::job_script_execution::emulator_with_s3;
use crate::wire::{call, error_code, error_message, ok};

/// The bucket the crawled data sits in.
const BUCKET: &str = "glue-crawler-stop-suite";

/// The modelled `CrawlerState` values this suite walks, written out here rather than read
/// from the crate's constants so the suite pins the wire values.
const READY: &str = "READY";
const RUNNING: &str = "RUNNING";
const STOPPING: &str = "STOPPING";

/// The modelled `LastCrawlStatus` of a crawl that was stopped.
const CANCELLED: &str = "CANCELLED";

/// The `Crawler` structure `GetCrawler` reports for `name`.
async fn reported(router: &MultiServiceRouter, name: &str) -> Value {
    ok(router, "GetCrawler", &json!({ "Name": name })).await["Crawler"].clone()
}

/// Stage the data, the database and one crawler over it, and answer the crawler's name.
async fn given_crawler_over_staged_data(router: &MultiServiceRouter, name: &str) {
    given_database(router, DATABASE).await;
    given_crawler_over(
        router,
        name,
        json!([{ "Path": format!("s3://{BUCKET}/data/") }]),
        json!({}),
    )
    .await;
}

/// A crawl under way is stopped: the crawler goes `RUNNING` -> `STOPPING` -> `READY`, the crawl
/// it stopped is reported `CANCELLED`, and a second stop while it is stopping is the modelled
/// `CrawlerStoppingException`.
#[tokio::test]
async fn a_crawl_under_way_is_stopped_and_comes_back_to_ready() {
    let (router, engine) = emulator_with_s3();
    stage(&router, BUCKET, &[("data/rows.csv", "id,name\n1,ada\n")]).await;
    let name = "stopped-crawler";
    given_crawler_over_staged_data(&router, name).await;

    ok(&router, "StartCrawler", &json!({ "Name": name })).await;
    // Hold the transient window open, so the crawl under way is what the stop is asked for.
    set_due_instant(&engine, far_future());
    assert_eq!(
        reported(&router, name).await["State"],
        json!(RUNNING),
        "a started crawl leaves the crawler RUNNING"
    );

    let (status, body) = call(&router, "StopCrawler", &json!({ "Name": name })).await;
    assert_eq!(status, 200, "StopCrawler answers 200, got {status} with {body}");
    assert_eq!(
        body,
        json!({}),
        "StopCrawler reports no member of its own, got {body}"
    );
    // The way back to READY is owed by the clock, so hold that window open too.
    set_due_instant(&engine, far_future());
    let stopping = reported(&router, name).await;
    assert_eq!(
        stopping["State"],
        json!(STOPPING),
        "a stopped crawl leaves the crawler STOPPING, got {stopping}"
    );

    let (status, body) = call(&router, "StopCrawler", &json!({ "Name": name })).await;
    assert_eq!(
        status, 400,
        "a crawler already stopping refuses a second stop, got {status} with {body}"
    );
    assert!(
        error_code(&body).ends_with("CrawlerStoppingException"),
        "expected CrawlerStoppingException, got {}",
        error_code(&body)
    );
    assert_eq!(
        error_message(&body),
        format!("Crawler with name {name} is stopping"),
        "the refusal names the crawler asked for, got {:?}",
        error_message(&body)
    );

    // The finish the clock owed falls due: the crawler is READY again and the crawl it was
    // asked to stop is reported as the cancelled one.
    set_due_instant(&engine, already_past());
    let settled = reported(&router, name).await;
    assert_eq!(
        settled["State"],
        json!(READY),
        "the stopped crawl comes to rest with the crawler READY, got {settled}"
    );
    assert_eq!(
        settled["LastCrawl"]["Status"],
        json!(CANCELLED),
        "the crawl that was stopped is reported CANCELLED, got {settled}"
    );

    let (status, body) = call(&router, "StopCrawler", &json!({ "Name": name })).await;
    assert_eq!(
        status, 400,
        "a crawler back at rest has no crawl to stop, got {status} with {body}"
    );
    assert!(
        error_code(&body).ends_with("CrawlerNotRunningException"),
        "expected CrawlerNotRunningException, got {}",
        error_code(&body)
    );
}

/// A crawler with no crawl under way, and a name no crawler has, each earn their own modelled
/// class — and the sentence names the crawler the request asked for.
///
/// The rows differ along why there is no crawl to stop: a crawler nothing ever started, one
/// whose crawl has already finished, and three names no crawler in the account has.
#[tokio::test]
async fn a_crawler_with_no_crawl_under_way_has_none_to_stop() {
    let (router, engine) = emulator_with_s3();
    stage(&router, BUCKET, &[("data/rows.csv", "id,name\n1,ada\n")]).await;
    given_database(&router, DATABASE).await;

    given_crawler_over_staged_data(&router, "never-started").await;
    given_crawler_over_staged_data(&router, "already-finished").await;
    ok(
        &router,
        "StartCrawler",
        &json!({ "Name": "already-finished" }),
    )
    .await;
    set_due_instant(&engine, already_past());
    assert_eq!(
        reported(&router, "already-finished").await["State"],
        json!(READY),
        "the crawl of the finished crawler must have come to rest"
    );

    for case in ["never-started", "already-finished"] {
        let (status, body) = call(&router, "StopCrawler", &json!({ "Name": case })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("CrawlerNotRunningException"),
            "case {case:?}: expected CrawlerNotRunningException, got {}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            format!("Crawler with name {case} is not running"),
            "case {case:?}: the refusal names the crawler asked for, got {:?}",
            error_message(&body)
        );
    }

    for case in [
        "no-such-crawler",
        "Crawler-With-Mixed-Case",
        "a-name.carrying-dots",
    ] {
        let (status, body) = call(&router, "StopCrawler", &json!({ "Name": case })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("EntityNotFoundException"),
            "case {case:?}: expected EntityNotFoundException, got {}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            format!("Crawler entry with name {case} does not exist"),
            "case {case:?}: the refusal names the crawler asked for, got {:?}",
            error_message(&body)
        );
    }
}
