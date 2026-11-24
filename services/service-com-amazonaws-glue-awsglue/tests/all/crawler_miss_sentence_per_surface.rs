// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH sentence each Glue surface answers a crawler miss with, across
//! every operation of this service that addresses a crawler by name.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! RULE under test: a refusal's message is the sentence the service words for the surface and
//! request form that answers THAT refusal, so where one service words a single error class
//! with several sentences, each surface renders its own and none may be served by another
//! surface's — whatever the request's variable part (the name it addressed) is, and whether or
//! not one sentence happens to be an extension of another.
//!
//! The rows below span what the rule generalises over: every operation of this service that
//! refuses an absent crawler (the crawler-definition reads, the definition write, the schedule
//! write, the run start, and the two schedule-state toggles), the request member each of them
//! addresses the crawler by, and the name itself (both published length bounds, a space, a tab,
//! a non-ASCII letter, and a name differing from a stored crawler only in case). The recorded
//! cases, `StartCrawlerSchedule` and `UpdateCrawler` on a name no crawler has, are two rows.
//!
//! The same rule across `TestConnection`'s two request forms is
//! `tests/all/test_connection_unified_only_wording.rs`, which spans every connection kind.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok};

/// The role the crawler this suite does hold carries.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/service-role/AWSGlueServiceRole-surfaces";

/// The crawler the store holds, so every refusal below is a miss inside a populated store.
const PRESENT: &str = "present-crawler";

/// Bounds of `NameString`, the shape every crawler-name member below targets: `length 1..255`
/// with a pattern that admits a space, a tab and non-ASCII letters (source:
/// `smithy_query shape=NameString`).
const NAME_MIN: usize = 1;
const NAME_MAX: usize = 255;

/// The sentence the crawler-DEFINITION surface words a miss with: the reads of the crawler
/// record itself, and the write that replaces its schedule.
fn definition_sentence(name: &str) -> String {
    format!("Crawler entry with name {name} does not exist")
}

/// The sentence the crawler-definition WRITE (`UpdateCrawler`) words the same miss with: it
/// states the miss over the member the request addressed the crawler by and names no value, so
/// it is the one sentence here with no variable part at all.
const DEFINITION_WRITE_SENTENCE: &str = "Crawler with provided name does not exist";

/// The sentence the RUN-start surface words the same miss with.
fn run_start_sentence(name: &str) -> String {
    format!("Crawler with name {name} does not exist")
}

/// The sentence the SCHEDULE-STATE toggles word the same miss with, which states the miss a
/// second time.
fn schedule_sentence(name: &str) -> String {
    format!("Crawler with name {name} does not exist not found.")
}

/// Which sentence a surface answers with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Surface {
    /// The crawler record's own reads, and the write that replaces its schedule.
    Definition,
    /// The write that replaces the crawler's definition.
    DefinitionWrite,
    /// Starting a crawl.
    RunStart,
    /// Putting a schedule back in force, or suspending it.
    ScheduleState,
}

impl Surface {
    /// The sentence this surface words the miss of `name` with.
    fn sentence(self, name: &str) -> String {
        match self {
            Self::Definition => definition_sentence(name),
            Self::DefinitionWrite => DEFINITION_WRITE_SENTENCE.to_string(),
            Self::RunStart => run_start_sentence(name),
            Self::ScheduleState => schedule_sentence(name),
        }
    }
}

/// Every operation of this service that refuses a crawler this account does not hold, with the
/// request member it addresses the crawler by and the surface it belongs to.
const CRAWLER_SURFACES: [(&str, &str, Surface); 9] = [
    ("GetCrawler", "Name", Surface::Definition),
    ("DeleteCrawler", "Name", Surface::Definition),
    ("UpdateCrawler", "Name", Surface::DefinitionWrite),
    ("StopCrawler", "Name", Surface::Definition),
    ("ListCrawls", "CrawlerName", Surface::Definition),
    ("UpdateCrawlerSchedule", "CrawlerName", Surface::Definition),
    ("StartCrawler", "Name", Surface::RunStart),
    ("StartCrawlerSchedule", "CrawlerName", Surface::ScheduleState),
    ("StopCrawlerSchedule", "CrawlerName", Surface::ScheduleState),
];

/// Create the crawler the store holds, scheduled, so every miss below is answered beside a
/// crawler that does resolve.
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

/// The names every row below addresses: absent, and spanning the length bounds, the characters
/// the name shape admits, and how close the name is to one the store does hold.
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
    ]
}

/// The message of one call that must be the modelled crawler miss, with the status and the
/// error class asserted here so each row only states the sentence it expects.
async fn miss_message(
    case: &str,
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> String {
    let (status, answer) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case}: expected {operation} to answer 400, got {status} with {answer}"
    );
    assert!(
        error_code(&answer).contains("EntityNotFoundException"),
        "case {case}: expected {operation} to answer EntityNotFoundException, got {answer}"
    );
    error_message(&answer)
}

/// Every surface answers the miss with ITS OWN sentence, built from the name the request sent,
/// whatever that name is.
#[tokio::test]
async fn every_crawler_surface_words_the_miss_its_own_way() {
    let router = emulator();
    given_present_crawler(&router).await;

    for (name_case, name) in absent_names() {
        for (operation, member, surface) in CRAWLER_SURFACES {
            let case = format!("{operation} with {name_case}");
            let actual = miss_message(&case, &router, operation, &json!({ member: &name })).await;
            let expected = surface.sentence(&name);
            assert_eq!(
                actual, expected,
                "case {case}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}

/// The sentences are DIFFERENT for one and the same absent name, and no surface is served by
/// another's: in particular the schedule toggles' sentence is not the run start's extended by
/// accident, and the definition write's names nothing where the reads' names the crawler — a
/// caller reading the whole message must see the surface's own text, and only the toggles
/// restate the miss.
#[tokio::test]
async fn the_surfaces_sentences_are_not_interchangeable() {
    let router = emulator();
    given_present_crawler(&router).await;

    let name = "absent-crawler";
    let definition = miss_message(
        "definition read",
        &router,
        "GetCrawler",
        &json!({ "Name": name }),
    )
    .await;
    let definition_write = miss_message(
        "definition write",
        &router,
        "UpdateCrawler",
        &json!({ "Name": name }),
    )
    .await;
    let run_start = miss_message(
        "run start",
        &router,
        "StartCrawler",
        &json!({ "Name": name }),
    )
    .await;
    let schedule = miss_message(
        "schedule toggle",
        &router,
        "StartCrawlerSchedule",
        &json!({ "CrawlerName": name }),
    )
    .await;

    for (left_case, left) in [
        ("definition read", &definition),
        ("definition write", &definition_write),
        ("run start", &run_start),
        ("schedule toggle", &schedule),
    ] {
        for (right_case, right) in [
            ("definition read", &definition),
            ("definition write", &definition_write),
            ("run start", &run_start),
            ("schedule toggle", &schedule),
        ] {
            if left_case == right_case {
                continue;
            }
            assert_ne!(
                left, right,
                "case {left_case} vs {right_case}: each surface words the miss its own way, got \
                 {left:?} for both"
            );
        }
    }

    // The definition write is the one sentence that discloses no part of the request.
    assert!(
        !definition_write.contains(name),
        "case definition write: its sentence names no requested crawler, got \
         {definition_write:?}"
    );
    for (case, other) in [
        ("definition read", &definition),
        ("run start", &run_start),
        ("schedule toggle", &schedule),
    ] {
        assert!(
            other.contains(name),
            "case {case}: its sentence names the crawler that was asked for, got {other:?}"
        );
    }

    // Only the schedule toggles restate the miss after naming the crawler.
    assert!(
        schedule.ends_with(" not found."),
        "case schedule toggle: the toggles restate the miss, got {schedule:?}"
    );
    for (case, other) in [
        ("definition read", &definition),
        ("definition write", &definition_write),
        ("run start", &run_start),
    ] {
        assert!(
            !other.ends_with(" not found."),
            "case {case}: only the schedule toggles restate the miss, got {other:?}"
        );
    }

    // Both toggles share the one sentence the schedule surface words, and the suspension is
    // not reworded on the resumption's evidence.
    let suspension = miss_message(
        "schedule suspension",
        &router,
        "StopCrawlerSchedule",
        &json!({ "CrawlerName": name }),
    )
    .await;
    assert_eq!(
        suspension, schedule,
        "case schedule suspension: both toggles word the miss the same way, got {suspension:?} \
         against {schedule:?}"
    );
}

/// None of these refusals writes: after every miss above the store still holds exactly the one
/// crawler it was given, with its schedule where it was, so a caller that probed an absent
/// name sees nothing new.
#[tokio::test]
async fn a_refused_call_leaves_the_store_as_it_was() {
    let router = emulator();
    given_present_crawler(&router).await;
    let before = ok(&router, "GetCrawler", &json!({ "Name": PRESENT })).await;

    for (operation, member, _) in CRAWLER_SURFACES {
        let case = format!("{operation} against an absent crawler");
        miss_message(
            &case,
            &router,
            operation,
            &json!({ member: "absent-crawler" }),
        )
        .await;

        // Nothing was created under the probed name.
        miss_message(
            &case,
            &router,
            "GetCrawler",
            &json!({ "Name": "absent-crawler" }),
        )
        .await;

        let after = ok(&router, "GetCrawler", &json!({ "Name": PRESENT })).await;
        assert_eq!(
            after, before,
            "case {case}: the refused call must leave the stored crawler unchanged, got {after} \
             after {before}"
        );
    }
}
