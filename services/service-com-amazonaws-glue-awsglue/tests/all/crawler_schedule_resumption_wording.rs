// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WORDING `StartCrawlerSchedule` refuses an absent crawler with,
//! beside the sentence the crawler-definition read (`GetCrawler`) answers for the same name.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! RULE under test: a refusal's message is the sentence the service words for THAT
//! operation's own refusal, never a sibling operation's sentence borrowed for it — so the
//! miss of the call that puts a schedule back in force states the addressed crawler's
//! absence and then the miss a second time, while the read of the crawler's definition
//! states its own, different sentence for the very same name. Both carry exactly one
//! variable part: the name the request sent, echoed as sent.
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
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/service-role/AWSGlueServiceRole-wording";

/// The crawler the store holds, so every refusal below is a miss inside a populated store
/// rather than the answer of an empty one.
const PRESENT: &str = "present-crawler";

/// Bounds of `NameString`, the shape `StartCrawlerScheduleRequest$CrawlerName` targets:
/// `len 1..255` with the pattern `^[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*$`
/// (source: `smithy_query shape=NameString`), which admits a space, a tab and non-ASCII
/// letters. The rows below sit on both published bounds of the length and inside it.
const NAME_MIN: usize = 1;
const NAME_MAX: usize = 255;

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

/// Putting back in force the schedule of a crawler this account does not hold states the
/// schedule surface's OWN miss: the requested name, that it does not exist, and the miss a
/// second time.
#[tokio::test]
async fn a_start_against_an_absent_crawler_states_the_schedule_surface_sentence() {
    let router = emulator();
    given_present_crawler(&router).await;

    for (case, name) in absent_names() {
        let answer = miss_body(
            &case,
            &router,
            "StartCrawlerSchedule",
            &json!({ "CrawlerName": &name }),
        )
        .await;
        let actual = error_message(&answer);
        let expected = format!("Crawler with name {name} does not exist not found.");
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
async fn the_definition_read_states_its_own_sentence_for_the_same_name() {
    let router = emulator();
    given_present_crawler(&router).await;

    for (case, name) in absent_names() {
        let start = error_message(
            &miss_body(
                &case,
                &router,
                "StartCrawlerSchedule",
                &json!({ "CrawlerName": &name }),
            )
            .await,
        );
        let read = error_message(
            &miss_body(&case, &router, "GetCrawler", &json!({ "Name": &name })).await,
        );

        let expected_read = format!("Crawler entry with name {name} does not exist");
        assert_eq!(
            read, expected_read,
            "case {case}: expected the definition read's own sentence {expected_read:?}, got \
             {read:?}"
        );
        assert_ne!(
            start, read,
            "case {case}: the schedule surface and the definition read word the miss \
             differently, got {start:?} for both"
        );
    }
}

/// A refused start writes nothing: the addressed name resolves to no crawler afterwards and
/// the crawler the store does hold keeps its schedule in force.
#[tokio::test]
async fn a_refused_start_leaves_the_store_as_it_was() {
    let router = emulator();
    given_present_crawler(&router).await;

    for (case, name) in absent_names() {
        miss_body(
            &case,
            &router,
            "StartCrawlerSchedule",
            &json!({ "CrawlerName": &name }),
        )
        .await;

        // Nothing was created under the requested name.
        miss_body(&case, &router, "GetCrawler", &json!({ "Name": &name })).await;

        let held = ok(&router, "GetCrawler", &json!({ "Name": PRESENT })).await;
        assert_eq!(
            held.pointer("/Crawler/Schedule/ScheduleExpression")
                .and_then(Value::as_str),
            Some("cron(15 12 * * ? *)"),
            "case {case}: the stored crawler keeps its schedule, got {held}"
        );
    }
}
