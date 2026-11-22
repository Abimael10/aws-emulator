// SPDX-License-Identifier: CC0-1.0

//! The WORDING of the refusals the crawler-RUN surface answers with: the miss `StartCrawler`
//! states for a crawler this account does not hold, and the conflict `DeleteCrawler` states
//! for a crawler whose crawl is in progress.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! RULE under test: a refusal's message is the sentence the service words for THAT
//! operation's own refusal, never a sibling operation's sentence borrowed for it — so the
//! start of a crawl states its own miss for an absent crawler while the read of the crawler's
//! definition states a different one for the very same name, and the delete of a crawler with
//! a run in progress states the delete it refuses rather than the sentence a write to the
//! crawler's definition earns. Each sentence carries exactly one variable part: the `Name` the
//! request sent, echoed as sent, the rest being fixed text.
//!
//! The rows below span what the rule generalises over — the requested name's length (one
//! character, and the longest the model admits), the characters in it (a space, a tab, a
//! non-ASCII letter, all admitted by `NameString`'s pattern) and how close it is to a name the
//! store does hold (differing only in case) — because each sentence is built from the
//! request's name whatever that name is. The recorded workflows' own calls are one row each.
//!
//! The due instant a crawl's finish falls at is moved THROUGH THE STORE
//! ([`crate::crawler_runs::set_due_instant`]) rather than slept for, so the window in which a
//! crawl is in progress is held open deterministically.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{CRAWLER_STATE_READY, CRAWLER_STATE_RUNNING};
use smithy_server_runtime::MultiServiceRouter;

use crate::crawler_runs::{
    already_past, emulator_with_s3, far_future, given_crawler_over, given_database,
    set_due_instant, stage, DATABASE, ROLE,
};
// The ONE helper every suite in this crate asserts a refusal's status and error class with,
// so the rows below only have to state the sentence each refusal is expected to word.
use crate::error_classification::refused;
use crate::wire::{emulator, error_message, ok};

/// The crawler the store holds in the miss tables below, so every refusal there is a miss
/// inside a populated store rather than the answer of an empty one.
const PRESENT: &str = "present-crawler";

/// Bounds of `NameString`, the shape `StartCrawlerRequest$Name` and
/// `DeleteCrawlerRequest$Name` both target: `len 1..255` with the pattern
/// `^[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*$` (source: `smithy_query
/// shape=NameString`), which admits a space, a tab and non-ASCII letters. The rows below sit
/// on both published bounds of the length and inside it.
const NAME_MIN: usize = 1;
const NAME_MAX: usize = 255;

/// The bucket and prefix every crawl here reads, so the crawler whose run is held in flight
/// has real data behind its target.
const BUCKET: &str = "crawler-run-wording";
const PREFIX: &str = "data/";

/// Create the crawler the miss tables hold, so the store is populated.
async fn given_present_crawler(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateCrawler",
        &json!({
            "Name": PRESENT,
            "Role": ROLE,
            "DatabaseName": DATABASE,
            "Targets": { "S3Targets": [{ "Path": format!("s3://{BUCKET}/{PREFIX}") }] },
        }),
    )
    .await;
}

/// The names the miss rows address: absent, and spanning the length bounds and the characters
/// the name shape admits.
fn absent_names() -> Vec<(String, String)> {
    vec![
        ("never created".to_string(), "absent-crawler".to_string()),
        (
            "the shortest name the model admits".to_string(),
            "x".repeat(NAME_MIN),
        ),
        (
            "the longest name the model admits".to_string(),
            "l".repeat(NAME_MAX),
        ),
        (
            "a name with a space".to_string(),
            "absent crawler".to_string(),
        ),
        (
            "a name with a tab".to_string(),
            "absent\tcrawler".to_string(),
        ),
        (
            "a non-ASCII name".to_string(),
            "crawler-ausgeführt-日本".to_string(),
        ),
        (
            "differs from a stored crawler only in case".to_string(),
            PRESENT.to_uppercase(),
        ),
        (
            "differs from a stored crawler in one letter's case".to_string(),
            "present-Crawler".to_string(),
        ),
    ]
}

/// The names the delete-while-running rows create their crawler under: the same axes, minus
/// the rows about a name nothing holds (this table's crawler is held, and crawling).
fn held_names() -> Vec<(String, String)> {
    vec![
        ("a plain name".to_string(), "crawling-crawler".to_string()),
        (
            "the shortest name the model admits".to_string(),
            "s".repeat(NAME_MIN),
        ),
        (
            "the longest name the model admits".to_string(),
            "n".repeat(NAME_MAX),
        ),
        (
            "a name with a space".to_string(),
            "crawling crawler".to_string(),
        ),
        (
            "a name with a tab".to_string(),
            "crawling\tcrawler".to_string(),
        ),
        (
            "a non-ASCII name".to_string(),
            "crawler-ausgeführt-日本".to_string(),
        ),
    ]
}

// ---------------------------------------------------------------------------
// Starting a crawl of a crawler this account does not hold
// ---------------------------------------------------------------------------

/// `StartCrawler` for an absent crawler states the RUN surface's own miss: the fixed text
/// followed by the requested name and the fixed tail.
#[tokio::test]
async fn starting_an_absent_crawler_states_the_run_surfaces_own_sentence() {
    let router = emulator();
    given_present_crawler(&router).await;

    for (case, name) in absent_names() {
        let answer = refused(
            &router,
            &case,
            "StartCrawler",
            &json!({ "Name": &name }),
            "EntityNotFoundException",
        )
        .await;
        let actual = error_message(&answer);
        let expected = format!("Crawler with name {name} does not exist");
        assert_eq!(
            actual, expected,
            "case {case}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// The very same name read through the crawler's DEFINITION answers that read's own,
/// different sentence — which is what makes each refusal the operation's own wording rather
/// than one template reused across the service.
#[tokio::test]
async fn the_definition_read_states_its_own_sentence_for_the_same_absent_name() {
    let router = emulator();
    given_present_crawler(&router).await;

    for (case, name) in absent_names() {
        let start = error_message(
            &refused(
                &router,
                &case,
                "StartCrawler",
                &json!({ "Name": &name }),
                "EntityNotFoundException",
            )
            .await,
        );
        let read = error_message(
            &refused(
                &router,
                &case,
                "GetCrawler",
                &json!({ "Name": &name }),
                "EntityNotFoundException",
            )
            .await,
        );

        let expected_read = format!("Crawler entry with name {name} does not exist");
        assert_eq!(
            read, expected_read,
            "case {case}: expected the definition read's own sentence {expected_read:?}, got \
             {read:?}"
        );
        assert_ne!(
            start, read,
            "case {case}: the run surface and the definition read word the miss differently, \
             got {start:?} for both"
        );
    }
}

/// A refused start writes nothing a later caller sees: the addressed name still resolves to no
/// crawler, and the crawler the store does hold is still at rest with no run on it.
#[tokio::test]
async fn a_refused_start_leaves_the_store_as_it_was() {
    let router = emulator();
    given_present_crawler(&router).await;

    for (case, name) in absent_names() {
        refused(
            &router,
            &case,
            "StartCrawler",
            &json!({ "Name": &name }),
            "EntityNotFoundException",
        )
        .await;

        // Nothing was created under the requested name.
        refused(
            &router,
            &case,
            "GetCrawler",
            &json!({ "Name": &name }),
            "EntityNotFoundException",
        )
        .await;

        let held = ok(&router, "GetCrawler", &json!({ "Name": PRESENT })).await;
        assert_eq!(
            held.pointer("/Crawler/State").and_then(Value::as_str),
            Some(CRAWLER_STATE_READY),
            "case {case}: the stored crawler is still at rest, got {held}"
        );
        assert!(
            held.pointer("/Crawler/LastCrawl").is_none(),
            "case {case}: no crawl ran, so the stored crawler reports none, got {held}"
        );
    }
}

// ---------------------------------------------------------------------------
// Deleting a crawler whose crawl is in progress
// ---------------------------------------------------------------------------

/// `DeleteCrawler` for a crawler with a run in progress states the DELETE it refuses: the
/// fixed text runs straight into the requested name, with no separating space, before the
/// fixed tail. The crawler survives the refusal, and the delete is served once the clock has
/// brought the crawl to rest.
#[tokio::test]
async fn deleting_a_crawling_crawler_states_the_delete_it_refuses() {
    for (case, name) in held_names() {
        let (router, engine) = emulator_with_s3();
        stage(&router, BUCKET, &[("data/rows.csv", "id\n1\n")]).await;
        given_database(&router, DATABASE).await;
        given_crawler_over(
            &router,
            &name,
            json!([{ "Path": format!("s3://{BUCKET}/{PREFIX}") }]),
            json!({}),
        )
        .await;

        ok(&router, "StartCrawler", &json!({ "Name": &name })).await;
        // Held open, so the window in which the crawl is in progress is observable without
        // sleeping for it.
        set_due_instant(&engine, far_future());
        let running = ok(&router, "GetCrawler", &json!({ "Name": &name })).await;
        assert_eq!(
            running.pointer("/Crawler/State").and_then(Value::as_str),
            Some(CRAWLER_STATE_RUNNING),
            "case {case}: the crawl is in progress, got {running}"
        );

        let answer = refused(
            &router,
            &case,
            "DeleteCrawler",
            &json!({ "Name": &name }),
            "CrawlerRunningException",
        )
        .await;
        let actual = error_message(&answer);
        let expected = format!("Cannot delete crawler{name} while it's running");
        assert_eq!(
            actual, expected,
            "case {case}: expected {expected:?}, got {actual:?}"
        );

        // The refusal removed nothing: the crawler is still there, still crawling.
        let survived = ok(&router, "GetCrawler", &json!({ "Name": &name })).await;
        assert_eq!(
            survived.pointer("/Crawler/State").and_then(Value::as_str),
            Some(CRAWLER_STATE_RUNNING),
            "case {case}: the refused delete leaves the crawler where it was, got {survived}"
        );

        // Once the finish the clock owed is due, the very same delete is served.
        set_due_instant(&engine, already_past());
        let deleted = ok(&router, "DeleteCrawler", &json!({ "Name": &name })).await;
        assert_eq!(
            deleted,
            json!({}),
            "case {case}: DeleteCrawler reports no member of its own, got {deleted}"
        );
        refused(
            &router,
            &case,
            "GetCrawler",
            &json!({ "Name": &name }),
            "EntityNotFoundException",
        )
        .await;
    }
}

/// The conflict a WRITE to the crawler's definition earns for the very same crawler is a
/// different sentence: neither operation borrows the other's wording.
#[tokio::test]
async fn the_definition_write_states_its_own_conflict_for_the_same_crawler() {
    let (router, engine) = emulator_with_s3();
    stage(&router, BUCKET, &[("data/rows.csv", "id\n1\n")]).await;
    given_database(&router, DATABASE).await;
    let name = "both-refusals";
    given_crawler_over(
        &router,
        name,
        json!([{ "Path": format!("s3://{BUCKET}/{PREFIX}") }]),
        json!({}),
    )
    .await;

    ok(&router, "StartCrawler", &json!({ "Name": name })).await;
    set_due_instant(&engine, far_future());

    let delete = error_message(
        &refused(
            &router,
            "a crawl in progress",
            "DeleteCrawler",
            &json!({ "Name": name }),
            "CrawlerRunningException",
        )
        .await,
    );
    let update = error_message(
        &refused(
            &router,
            "a crawl in progress",
            "UpdateCrawler",
            &json!({ "Name": name }),
            "CrawlerRunningException",
        )
        .await,
    );

    let expected_delete = format!("Cannot delete crawler{name} while it's running");
    assert_eq!(
        delete, expected_delete,
        "case \"a crawl in progress\": expected the delete's own sentence {expected_delete:?}, \
         got {delete:?}"
    );
    assert_ne!(
        delete, update,
        "case \"a crawl in progress\": the delete and the definition write word the conflict \
         differently, got {delete:?} for both"
    );
}
