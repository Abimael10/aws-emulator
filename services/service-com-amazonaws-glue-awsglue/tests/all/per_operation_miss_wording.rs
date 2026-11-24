// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SENTENCE four Glue operations state a refusal with when the
//! entity the request addressed is not there to serve it.
//!
//! RULE: a refusal's `Message` is the wording the service states for THAT operation's error
//! class — its fixed text reproduced exactly, carrying only the variable parts that sentence
//! names and taking each of them from the request or from the store — so two operations that
//! refuse one and the same absent entity do not share one sentence, and a sentence with no
//! variable part never varies with the request. The status and the error code are the same
//! across a whole family (`EntityNotFoundException`, "a specified entity does not exist" —
//! `smithy_query operation=GetBlueprintRun,GetBlueprintRuns,UpdateCrawler`), which is exactly
//! why the sentence is what tells a client what was missing.
//!
//! The four operations here are the ones whose sentence this suite pins:
//!
//! * `GetBlueprintRun` addresses a RUN, whose address is the blueprint name AND the run id,
//!   so every address it resolves no run at reports both halves — whether the blueprint is
//!   absent or merely holds no run under that id.
//! * `GetBlueprintRuns` reads a run HISTORY and answers the service's generic absent-entity
//!   sentence, which names nothing of the request.
//! * `UpdateCrawler` writes a crawler's definition and states the miss over the member it was
//!   addressed by, where the definition READS name the crawler and the start of a crawl words
//!   it a third way.
//! * `StopColumnStatisticsTaskRun` states that no run of the addressed database and table
//!   stands in the state a stop acts on, naming neither of them.
//!
//! Each suite is a table whose rows differ along what the rule generalises over: the
//! requested name or id's value, length, casing and character set, whether the half of an
//! address that holds nothing is the parent or the child, and how a resource came to have
//! nothing to report. The expected strings are written out as literals rather than read from
//! the service's own helpers, so this suite pins the wording instead of restating it.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::blueprint_runs::{emulator_with_engine, get_run, register_active, start};
use crate::column_statistics_task_runs::{
    already_past, emulator_with_engine as column_statistics_emulator, far_future, set_due_instant,
};
use crate::error_classification::refused;
use crate::wire::{call, emulator, error_code, error_message, ok, ACCOUNT_ID};

/// The role a blueprint run is started as: `OrchestrationIAMRoleArn`, pattern
/// `^arn:aws[^:]*:iam::[0-9]*:role/.+$` (`smithy_query operation=StartBlueprintRun`).
const BLUEPRINT_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/GlueBlueprintRole";

/// The role a crawler carries: `Role`, "the IAM role ... used by the crawler to access
/// customer resources" (`smithy_query operation=CreateCrawler`).
const CRAWLER_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/service-role/AWSGlueServiceRole";

/// The longest `RunId` the single-run read admits: `IdString`, "len 1..255" (`smithy_query
/// operation=GetBlueprintRun`).
const MAX_RUN_ID_LENGTH: usize = 255;

/// The longest `BlueprintName` the single-run read admits: `OrchestrationNameString`,
/// "len 1..128", pattern `^[\.\-_A-Za-z0-9]+$` (`smithy_query operation=GetBlueprintRun`).
const MAX_ORCHESTRATION_NAME_LENGTH: usize = 128;

/// The longest `BlueprintName` the run-history read admits: `NameString`, "len 1..255"
/// (`smithy_query operation=GetBlueprintRuns`), which is also the `Name` the crawler write
/// admits (`smithy_query operation=UpdateCrawler`).
const MAX_NAME_LENGTH: usize = 255;

/// The error code every blueprint and crawler refusal below carries.
const NOT_FOUND: &str = "EntityNotFoundException";

/// The whole sentence `GetBlueprintRun` reports a miss with: fixed text around the two
/// variable parts, which are the run id and the blueprint name the request itself sent.
fn expected_run_miss(blueprint_name: &str, run_id: &str) -> String {
    format!("Blueprint run with id:{run_id} and blueprint name:{blueprint_name} not found")
}

/// The whole sentence `GetBlueprintRuns` reports a miss with: the service's generic
/// absent-entity text, with no variable part at all.
const EXPECTED_RUN_HISTORY_MISS: &str = "Entity not found";

/// The whole sentence `UpdateCrawler` reports a miss with: it states the miss over the member
/// the request addressed the crawler by and discloses no value.
const EXPECTED_CRAWLER_WRITE_MISS: &str = "Crawler with provided name does not exist";

/// The whole sentence the crawler-definition READS report the same miss with, which names the
/// crawler that was asked for.
fn expected_crawler_read_miss(name: &str) -> String {
    format!("Crawler entry with name {name} does not exist")
}

/// The whole sentence the start of a crawl reports the same miss with — a third wording for
/// one and the same absent name.
fn expected_crawl_start_miss(name: &str) -> String {
    format!("Crawler with name {name} does not exist")
}

/// The whole sentence `StopColumnStatisticsTaskRun` reports with when no run of the addressed
/// database and table stands in the state a stop acts on: fixed text naming the members the
/// request addressed the run by rather than the values it sent.
const EXPECTED_NOTHING_TO_STOP: &str =
    "No task found in Running state for input database and table name combination";

/// Assert that `operation` refused `request` with HTTP 400, `code`, and `expected`
/// verbatim, and answer the refusal's `Message` so a suite can assert further on what the
/// wire actually carried.
///
/// The status and the error class are asserted through [`crate::error_classification::refused`],
/// the one copy this crate's refusal-wording suites share, so only the sentence is checked
/// here.
async fn assert_refusal(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
    code: &str,
    expected: &str,
) -> String {
    let body = refused(router, case, operation, request, code).await;
    assert_eq!(
        error_message(&body),
        expected,
        "case {case:?} via {operation}: expected Message {expected:?}, got {:?}",
        error_message(&body)
    );
    error_message(&body)
}

// ---------------------------------------------------------------------------
// GetBlueprintRun: the run's whole address, whichever half holds nothing
// ---------------------------------------------------------------------------

/// Every address the single-run read resolves no run at is reported with that run's whole
/// address — the run id and the blueprint name the request sent — and the run that IS held
/// still reads back, so each row below is a miss and not a broken lookup.
#[tokio::test]
async fn the_single_run_read_reports_the_runs_whole_address() {
    let (router, engine) = emulator_with_engine();
    register_active(&router, &engine, "holder").await;
    register_active(&router, &engine, "other.holder-1").await;
    let held = start(
        &router,
        &json!({ "BlueprintName": "holder", "RoleArn": BLUEPRINT_ROLE_ARN }),
    )
    .await;

    let longest_run_id = "r".repeat(MAX_RUN_ID_LENGTH);
    let longest_blueprint = "n".repeat(MAX_ORCHESTRATION_NAME_LENGTH);

    // (case, the blueprint the read names, the run id it asks for)
    let rows: Vec<(&str, &str, String)> = vec![
        (
            "an id nothing minted, under a registered blueprint",
            "holder",
            "br_0000000000000000".to_string(),
        ),
        (
            "a held id under the other registered blueprint",
            "other.holder-1",
            held.clone(),
        ),
        (
            "a prefix of a held id",
            "holder",
            held[..held.len() - 1].to_string(),
        ),
        (
            "a held id with one character added",
            "holder",
            format!("{held}0"),
        ),
        ("the shortest id the request admits", "holder", "r".to_string()),
        (
            "the longest id the request admits",
            "holder",
            longest_run_id,
        ),
        (
            "a blueprint nothing registered",
            "absent-blueprint",
            "br_0000000000000000".to_string(),
        ),
        (
            "a blueprint name at the published ceiling, nothing registered",
            longest_blueprint.as_str(),
            "br_0000000000000000".to_string(),
        ),
    ];

    for (case, blueprint, run_id) in &rows {
        let expected = expected_run_miss(blueprint, run_id);
        let reported = assert_refusal(
            &router,
            case,
            "GetBlueprintRun",
            &json!({ "BlueprintName": blueprint, "RunId": run_id }),
            NOT_FOUND,
            &expected,
        )
        .await;
        // Both halves of the address the read could not resolve are in the sentence the WIRE
        // carried, which is what tells a client an unknown run from an unknown blueprint.
        assert!(
            reported.contains(*blueprint) && reported.contains(run_id.as_str()),
            "case {case:?}: the sentence must carry both halves of the address, got {reported:?}"
        );
    }

    // The control: the run the start recorded still reads back under its own blueprint.
    let read = get_run(&router, "holder", &held).await;
    assert_eq!(
        read["RunId"].as_str(),
        Some(held.as_str()),
        "case \"the held run\": the stored run must still be reported, got {read}"
    );
}

// ---------------------------------------------------------------------------
// GetBlueprintRuns: one fixed sentence, naming nothing
// ---------------------------------------------------------------------------

/// The run-history read answers a blueprint this account holds none of with its own fixed
/// sentence, which carries no part of the request, and a blueprint it DOES hold is served.
#[tokio::test]
async fn the_run_history_read_reports_one_fixed_sentence() {
    let (router, engine) = emulator_with_engine();
    register_active(&router, &engine, "registered").await;

    let longest_name = "n".repeat(MAX_NAME_LENGTH);

    // The rows differ along how the requested name misses: never registered, differing from a
    // registered name only in case, a single character, punctuation and digits, and the
    // longest name the request admits.
    let rows: Vec<(&str, &str)> = vec![
        ("never registered", "absent"),
        ("differs only in case", "REGISTERED"),
        ("a single character", "x"),
        ("punctuation and digits", "team.data_lake-2026"),
        (
            "at the published name ceiling",
            longest_name.as_str(),
        ),
    ];

    for (case, name) in &rows {
        let reported = assert_refusal(
            &router,
            case,
            "GetBlueprintRuns",
            &json!({ "BlueprintName": name }),
            NOT_FOUND,
            EXPECTED_RUN_HISTORY_MISS,
        )
        .await;
        assert!(
            !reported.contains(name),
            "case {case:?}: a fixed sentence must disclose no requested name, got {reported:?}"
        );
    }

    // The control: the registered blueprint's history is served, so the refusals above are
    // about the name each row asked for.
    let page = ok(
        &router,
        "GetBlueprintRuns",
        &json!({ "BlueprintName": "registered" }),
    )
    .await;
    assert!(
        page.get("BlueprintRuns").and_then(Value::as_array).is_some(),
        "case \"the registered blueprint\": its history must be served, got {page}"
    );
}

// ---------------------------------------------------------------------------
// UpdateCrawler: the definition write's own sentence
// ---------------------------------------------------------------------------

/// Store one crawler, so a row that misses is a miss of a name and not of an empty service.
async fn given_crawler(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateCrawler",
        &json!({
            "Name": name,
            "Role": CRAWLER_ROLE_ARN,
            "DatabaseName": "crawled",
            "Targets": { "S3Targets": [{ "Path": "s3://crawler-bucket/data/" }] },
        }),
    )
    .await;
}

/// The crawler-definition WRITE states the miss over the member it was addressed by and
/// discloses no value — a different sentence from the definition reads' and from the start of
/// a crawl's, for one and the same absent name.
#[tokio::test]
async fn the_crawler_definition_write_states_its_own_miss() {
    let router = emulator();
    given_crawler(&router, "present-crawler").await;

    let longest_name = "n".repeat(MAX_NAME_LENGTH);

    // The rows differ along how the requested name misses: never created, differing from a
    // stored crawler only in case (the model documents no case fold for a crawler name), a
    // single character, punctuation and digits, and the longest name the request admits.
    let rows: Vec<(&str, &str)> = vec![
        ("never created", "absent-crawler"),
        ("differs only in case", "PRESENT-CRAWLER"),
        ("differs only in one letter's case", "present-Crawler"),
        ("a single character", "c"),
        ("punctuation and digits", "team.data_lake-2026"),
        ("at the published name ceiling", longest_name.as_str()),
    ];

    for (case, name) in &rows {
        let reported = assert_refusal(
            &router,
            case,
            "UpdateCrawler",
            &json!({ "Name": name, "TablePrefix": "nowhere_" }),
            NOT_FOUND,
            EXPECTED_CRAWLER_WRITE_MISS,
        )
        .await;
        assert!(
            !reported.contains(name),
            "case {case:?}: the write's sentence must disclose no requested name, got \
             {reported:?}"
        );

        // The two other surfaces that address the SAME absent name each state their own
        // sentence, so no one of the three is borrowed for another.
        assert_refusal(
            &router,
            case,
            "GetCrawler",
            &json!({ "Name": name }),
            NOT_FOUND,
            &expected_crawler_read_miss(name),
        )
        .await;
        assert_refusal(
            &router,
            case,
            "StartCrawler",
            &json!({ "Name": name }),
            NOT_FOUND,
            &expected_crawl_start_miss(name),
        )
        .await;
    }

    // The control: the stored crawler's definition write is served and what it wrote reads
    // back, so the refusals above are about the name each row asked for.
    ok(
        &router,
        "UpdateCrawler",
        &json!({ "Name": "present-crawler", "TablePrefix": "written_" }),
    )
    .await;
    let read = ok(&router, "GetCrawler", &json!({ "Name": "present-crawler" })).await;
    assert_eq!(
        read["Crawler"]["TablePrefix"].as_str(),
        Some("written_"),
        "case \"the stored crawler\": the served write must read back, got {read}"
    );
}

// ---------------------------------------------------------------------------
// StopColumnStatisticsTaskRun: nothing in the state a stop acts on
// ---------------------------------------------------------------------------

/// Store `database` holding `table` with two columns, so a stop addressed at it resolves the
/// table and the only question left is whether a run of it is in flight.
async fn given_table(router: &MultiServiceRouter, database: &str, table: &str) {
    let (status, body) = call(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": database } }),
    )
    .await;
    assert!(
        status == 200 || error_code(&body).ends_with("AlreadyExistsException"),
        "CreateDatabase must answer 200 or report the database is stored, got {status} with \
         {body}"
    );
    ok(
        router,
        "CreateTable",
        &json!({
            "DatabaseName": database,
            "TableInput": {
                "Name": table,
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": format!("s3://bucket/{table}/"),
                    "Columns": [
                        { "Name": "a", "Type": "string" },
                        { "Name": "b", "Type": "string" },
                    ],
                },
            },
        }),
    )
    .await;
}

/// A stop of a stored table with no statistics run in the state a stop acts on is refused
/// with that error class's own fixed sentence, which names neither the database nor the
/// table — however the table came to have nothing to stop.
#[tokio::test]
async fn a_stop_with_nothing_running_states_the_running_state_miss() {
    let (router, engine) = column_statistics_emulator();
    let database = "stats_db";
    for table in ["never_run", "already_stopped", "clock_finished"] {
        given_table(&router, database, table).await;
    }

    // A run this test stops itself: the due instant is held open through the store, so the
    // stop lands while the run is in flight rather than after a span of wall clock.
    ok(
        &router,
        "StartColumnStatisticsTaskRun",
        &json!({
            "DatabaseName": database,
            "TableName": "already_stopped",
            "Role": CRAWLER_ROLE_ARN,
        }),
    )
    .await;
    set_due_instant(&engine, far_future());
    ok(
        &router,
        "StopColumnStatisticsTaskRun",
        &json!({ "DatabaseName": database, "TableName": "already_stopped" }),
    )
    .await;

    // A run the CLOCK finished: its due instant is already past, so the next call
    // materialises the finish the clock owed it and the run is no longer in flight.
    ok(
        &router,
        "StartColumnStatisticsTaskRun",
        &json!({
            "DatabaseName": database,
            "TableName": "clock_finished",
            "Role": CRAWLER_ROLE_ARN,
        }),
    )
    .await;
    set_due_instant(&engine, already_past());

    // (case, the stop request)
    let rows: Vec<(&str, Value)> = vec![
        (
            "a table no run was ever started of",
            json!({ "DatabaseName": database, "TableName": "never_run" }),
        ),
        (
            "a second stop of a run this caller stopped",
            json!({ "DatabaseName": database, "TableName": "already_stopped" }),
        ),
        (
            "a run the clock had already finished",
            json!({ "DatabaseName": database, "TableName": "clock_finished" }),
        ),
        (
            "the table addressed in another casing",
            json!({ "DatabaseName": database, "TableName": "NEVER_RUN" }),
        ),
        (
            "the catalog named explicitly",
            json!({
                "DatabaseName": database,
                "TableName": "never_run",
                "CatalogID": ACCOUNT_ID,
            }),
        ),
    ];

    for (case, request) in &rows {
        let reported = assert_refusal(
            &router,
            case,
            "StopColumnStatisticsTaskRun",
            request,
            "ColumnStatisticsTaskNotRunningException",
            EXPECTED_NOTHING_TO_STOP,
        )
        .await;
        let table = request["TableName"].as_str().unwrap_or_default();
        assert!(
            !reported.contains(database) && !reported.contains(table),
            "case {case:?}: the sentence must name neither the database nor the table, got \
             {reported:?}"
        );
    }

    // The control: a run started again of the same table IS in flight, so the rows above are
    // about what the table holds and not a stop that never works.
    ok(
        &router,
        "StartColumnStatisticsTaskRun",
        &json!({
            "DatabaseName": database,
            "TableName": "never_run",
            "Role": CRAWLER_ROLE_ARN,
        }),
    )
    .await;
    set_due_instant(&engine, far_future());
    ok(
        &router,
        "StopColumnStatisticsTaskRun",
        &json!({ "DatabaseName": database, "TableName": "never_run" }),
    )
    .await;
}
