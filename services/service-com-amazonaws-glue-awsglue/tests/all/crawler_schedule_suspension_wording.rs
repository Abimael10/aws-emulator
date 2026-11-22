// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WORDING the crawler schedule-STATE toggles refuse an absent
//! crawler with, beside the sentence the crawler-definition read (`GetCrawler`) answers for
//! the very same name.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! RULE under test: the schedule-STATE toggles — the call that suspends a crawler's
//! schedule and the call that puts it back in force — refuse a `CrawlerName` no crawler in
//! this account holds with ONE sentence, the one the schedule surface words: the requested
//! name, that it does not exist, and then the miss a second time. The crawler-definition
//! read words its own, different sentence for the same name, so neither template is
//! borrowed for the other surface. Both sentences carry exactly one variable part: the name
//! the request sent, echoed as sent.
//!
//! What each toggle answers for a table of absent names, and that a refused call writes
//! nothing, is asserted by the two toggles' own suites
//! ([`crate::crawler_schedule_suspension`] and [`crate::crawler_schedule_resumption`]);
//! what is left to this suite, and belongs to neither alone, is that the two AGREE on one
//! sentence while the definition read does not.
//!
//! The rows below span what the rule generalises over — the length of the requested name
//! (one character, and the longest the model admits), the characters in it (a space, a tab,
//! a non-ASCII letter, as `NameString`'s pattern admits) and how close it is to a name the
//! store does hold (differing only in case) — because the sentence is built from the
//! request's name whatever that name is. The recorded workflow's own call is one row.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok};

/// The role the crawler this suite does hold carries.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/service-role/AWSGlueServiceRole-toggle";

/// The crawler the store holds, scheduled, so every refusal below is a miss inside a
/// populated store rather than the answer of an empty one.
const PRESENT: &str = "present-crawler";

/// Bounds of `NameString`, the shape `StopCrawlerScheduleRequest$CrawlerName` and
/// `StartCrawlerScheduleRequest$CrawlerName` target: `len 1..255` with the pattern
/// `^[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*$` (source: `smithy_query
/// shape=NameString`), which admits a space, a tab and non-ASCII letters. The rows below sit
/// on both published bounds of the length and inside it.
const NAME_MIN: usize = 1;
const NAME_MAX: usize = 255;

/// The two operations that decide only a crawler's schedule STATE, which the rule covers.
const SCHEDULE_TOGGLES: &[&str] = &["StopCrawlerSchedule", "StartCrawlerSchedule"];

/// Create the crawler the store holds, scheduled, so the suite's own reads have something
/// to resolve.
async fn given_present_crawler(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateCrawler",
        &json!({
            "Name": PRESENT,
            "Role": ROLE_ARN,
            "DatabaseName": "analytics",
            "Targets": { "S3Targets": [{ "Path": "s3://crawler-bucket/data/" }] },
            "Schedule": "cron(15 12 * * ? *)",
        }),
    )
    .await;
}

/// The names every row below addresses: absent, and spanning the length bounds and the
/// characters the name shape admits.
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

/// The refusal body of one call that must be the modelled miss, with the status and the
/// error class asserted here so each row below only has to state the sentence it expects.
async fn miss_body(case: &str, router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, answer) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case}: expected {operation} to answer 400, got {status} with {answer}"
    );
    assert!(
        error_code(&answer).contains("EntityNotFoundException"),
        "case {case}: expected {operation} to answer EntityNotFoundException, got {answer}"
    );
    answer
}

/// Both schedule-state toggles word the SAME miss for the same absent name — the requested
/// name, that it does not exist, and the miss a second time — because the sentence belongs
/// to the surface that answers it and not to either operation alone.
#[tokio::test]
async fn both_schedule_toggles_word_one_miss_for_the_same_name() {
    let router = emulator();
    given_present_crawler(&router).await;

    for (case, name) in absent_names() {
        let expected = format!("Crawler with name {name} does not exist not found.");
        for operation in SCHEDULE_TOGGLES {
            let actual = error_message(
                &miss_body(&case, &router, operation, &json!({ "CrawlerName": &name })).await,
            );
            assert_eq!(
                actual, expected,
                "case {case} through {operation}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}

/// The very same name read through the crawler's DEFINITION answers that read's own,
/// different sentence — which is what makes each refusal the answering surface's wording
/// rather than one template reused across the service.
#[tokio::test]
async fn the_definition_read_states_its_own_sentence_for_the_same_name() {
    let router = emulator();
    given_present_crawler(&router).await;

    for (case, name) in absent_names() {
        let read =
            error_message(&miss_body(&case, &router, "GetCrawler", &json!({ "Name": &name })).await);
        let expected_read = format!("Crawler entry with name {name} does not exist");
        assert_eq!(
            read, expected_read,
            "case {case}: expected the definition read's own sentence {expected_read:?}, got \
             {read:?}"
        );

        for operation in SCHEDULE_TOGGLES {
            let toggle = error_message(
                &miss_body(&case, &router, operation, &json!({ "CrawlerName": &name })).await,
            );
            assert_ne!(
                toggle, read,
                "case {case}: {operation} and the definition read must word the miss \
                 differently, got {toggle:?} for both"
            );
        }
    }
}
