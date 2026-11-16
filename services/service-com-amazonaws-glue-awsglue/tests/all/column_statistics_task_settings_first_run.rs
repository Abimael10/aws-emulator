// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the two ends of the column-statistics task-SETTINGS sub-resource
//! that a client can only observe through another operation: the run a create with no
//! `Schedule` starts, as `GetColumnStatisticsTaskRuns` lists it, and the ONE wording every
//! miss on that sub-resource is refused with, whichever half of its address missed.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rules under test:
//!
//! * A create of the settings of a table that states NO schedule registers the
//!   configuration AND starts the first run it configures, so the run listing of that table
//!   reports exactly one run carrying the configuration the create stated, in a
//!   `ColumnStatisticsState` that still owes a further state; while that run is in flight a
//!   second create for the table is the modelled `ColumnStatisticsTaskRunningException` and
//!   persists nothing; and once the run is no longer in flight — by the clock or by a stop —
//!   the create is served and starts a run of its own. A create that states a schedule
//!   starts none: the schedule is when its runs are due.
//! * A settings entry is addressed by the (database, table) pair as ONE sub-resource, so
//!   every operation that addresses it answers the same miss, in the same fixed wording,
//!   whichever half of the pair the catalog holds nothing under — and that wording carries
//!   neither of the two names the request addressed the entry by.
//!
//! The recorded cases — a create with one role whose run a second create collides with, and
//! a delete for a pair naming nothing the catalog holds — are ONE row each of the tables
//! below; the rows differ along what the create configures (the role, the column list, each
//! published `SampleSize` bound, a security configuration), along which operation addresses
//! the entry, and along HOW the address misses.
//!
//! The due instant the run lifecycle turns on is moved THROUGH THE STORE rather than slept
//! for, so the in-flight window is deterministic.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::messages::{
    COLUMN_STATISTICS_TASK_SETTINGS_NOT_FOUND, COLUMN_STATISTICS_TASK_SETTINGS_RUN_ALREADY_STARTED,
};
use service_com_amazonaws_glue_awsglue::state::{
    COLUMN_STATISTICS_TASK_RUN_STATE_STOPPED, TERMINAL_COLUMN_STATISTICS_TASK_RUN_STATES,
};
use smithy_server_runtime::MultiServiceRouter;

// The settings suite owns the helpers that store a table and write and read an entry, the run
// suite owns the clock helpers and the readers of a reported run's members, and the history
// suite owns the readers of the listing: one copy of each drives every suite, so what this one
// creates and reads is exactly what those ones create and read.
use crate::column_statistics_task_run_history::{reported_runs, status_of};
use crate::column_statistics_task_runs::{
    already_past, emulator_with_engine, far_future, reported_columns, reported_number,
    set_due_instant,
};
use crate::column_statistics_task_settings::{
    create_body, get_settings, read_body, store_table, DAILY_SCHEDULE, ROLE,
};
use crate::wire::{call, emulator_on, error_code, error_message, ok, ACCOUNT_ID};

/// A second role, so a create that follows one of [`ROLE`]'s runs states a configuration of
/// its own: "the role used for running the column statistics".
const OTHER_ROLE: &str = "arn:aws:iam::000000000000:role/glue-column-stats-role-2";

// The emulator and the engine it runs on come from the run suite's own constructor
// (`emulator_with_engine`), so this suite holds the in-flight window open through the same
// seam every other column-statistics suite uses.

/// The `ColumnStatisticsTaskRuns` the listing reports for one table, newest first. The
/// member is reported even when the table has no run, which the operation models no miss
/// for.
async fn runs_of(router: &MultiServiceRouter, database: &str, table: &str) -> Vec<Value> {
    let page = ok(
        router,
        "GetColumnStatisticsTaskRuns",
        &json!({ "DatabaseName": database, "TableName": table }),
    )
    .await;
    reported_runs(&page)
}

/// The one run the listing of `table` reports, asserting there is exactly one.
async fn the_only_run_of(
    router: &MultiServiceRouter,
    case: &str,
    database: &str,
    table: &str,
) -> Value {
    let runs = runs_of(router, database, table).await;
    assert_eq!(
        runs.len(),
        1,
        "case {case:?}: expected exactly one run of the table, got {runs:?}"
    );
    runs[0].clone()
}

/// Assert that `run` has not finished: its status is none of the states a run never moves
/// out of again, so it still owes a further state.
fn assert_in_flight(case: &str, run: &Value) {
    let status = status_of(run);
    assert!(
        !TERMINAL_COLUMN_STATISTICS_TASK_RUN_STATES.contains(&status.as_str()),
        "case {case:?}: a run that has just been started still owes a further state, got \
         {status:?} in {run}"
    );
}

// The status a reported run stands in, the `ColumnNameList` it carries and a numeric member of
// one are read through the suites that own those readers (`status_of`, `reported_columns`,
// `reported_number`), so this suite derives none of them a second time.

// ---------------------------------------------------------------------------
// The run a create with no schedule starts
// ---------------------------------------------------------------------------

/// One row of the table below: a stored table, the create of its settings, and what the run
/// that create started must carry. A named structure rather than a tuple, so every column of
/// the table reads as what it is.
struct FirstRunCase<'a> {
    /// What this row varies, named in each of its assertions.
    case: &'a str,
    table: &'a str,
    /// The columns the stored table declares.
    columns: &'a [&'a str],
    /// The create members beyond `DatabaseName` and `TableName`, the `Role` included.
    extra: Value,
    /// The role the started run must report.
    expected_role: &'a str,
    /// The columns the started run must cover.
    expected_columns: Vec<&'a str>,
    /// The `SampleSize` the started run must report.
    expected_sample: f64,
    /// The security configuration the started run must report, if any.
    expected_security_configuration: Option<&'a str>,
}

/// A create that states no `Schedule` starts the first run its settings configure: the run
/// listing of that table reports exactly one run, carrying the configuration the create
/// stated, in a state that still owes a further one — and the settings read reports that
/// same run as the table's last execution attempt.
#[tokio::test]
async fn a_create_with_no_schedule_starts_one_run_carrying_what_it_configured() {
    let (router, engine) = emulator_with_engine();
    let database = "first_run_db";

    let cases: Vec<FirstRunCase<'_>> = vec![
        FirstRunCase {
            // The recorded case: only the three `@required` members, so the run covers
            // every column the stored table declares.
            case: "only the required members",
            table: "required_only",
            columns: &["id", "amount"],
            extra: json!({ "Role": ROLE }),
            expected_role: ROLE,
            expected_columns: vec!["id", "amount"],
            expected_sample: 0.0,
            expected_security_configuration: None,
        },
        FirstRunCase {
            case: "another role than the first row's",
            table: "other_role",
            columns: &["id"],
            extra: json!({ "Role": OTHER_ROLE }),
            expected_role: OTHER_ROLE,
            expected_columns: vec!["id"],
            expected_sample: 0.0,
            expected_security_configuration: None,
        },
        FirstRunCase {
            case: "an explicit column list of one of several columns",
            table: "one_column",
            columns: &["id", "amount", "country"],
            extra: json!({ "Role": ROLE, "ColumnNameList": ["amount"] }),
            expected_role: ROLE,
            expected_columns: vec!["amount"],
            expected_sample: 0.0,
            expected_security_configuration: None,
        },
        FirstRunCase {
            case: "an explicit column list of several columns",
            table: "several_columns",
            columns: &["id", "amount", "country"],
            extra: json!({ "Role": ROLE, "ColumnNameList": ["country", "id"] }),
            expected_role: ROLE,
            expected_columns: vec!["country", "id"],
            expected_sample: 0.0,
            expected_security_configuration: None,
        },
        // `SampleSize` is `range 0..100`
        // (`smithy_query operation=CreateColumnStatisticsTaskSettings`): each published
        // bound and one in between configure the run the same way.
        FirstRunCase {
            case: "the smallest sample share the model admits",
            table: "sample_low",
            columns: &["id"],
            extra: json!({ "Role": ROLE, "SampleSize": 0.0 }),
            expected_role: ROLE,
            expected_columns: vec!["id"],
            expected_sample: 0.0,
            expected_security_configuration: None,
        },
        FirstRunCase {
            case: "a sample share between the published bounds",
            table: "sample_middle",
            columns: &["id"],
            extra: json!({ "Role": ROLE, "SampleSize": 50.0 }),
            expected_role: ROLE,
            expected_columns: vec!["id"],
            expected_sample: 50.0,
            expected_security_configuration: None,
        },
        FirstRunCase {
            case: "the largest sample share the model admits",
            table: "sample_high",
            columns: &["id"],
            extra: json!({ "Role": ROLE, "SampleSize": 100.0 }),
            expected_role: ROLE,
            expected_columns: vec!["id"],
            expected_sample: 100.0,
            expected_security_configuration: None,
        },
        FirstRunCase {
            case: "a named security configuration",
            table: "secured",
            columns: &["id", "amount"],
            extra: json!({ "Role": ROLE, "SecurityConfiguration": "stats-logs" }),
            expected_role: ROLE,
            expected_columns: vec!["id", "amount"],
            expected_sample: 0.0,
            expected_security_configuration: Some("stats-logs"),
        },
        FirstRunCase {
            case: "a create naming the account's own catalog explicitly",
            table: "named_catalog",
            columns: &["id"],
            extra: json!({ "Role": ROLE, "CatalogID": ACCOUNT_ID }),
            expected_role: ROLE,
            expected_columns: vec!["id"],
            expected_sample: 0.0,
            expected_security_configuration: None,
        },
    ];

    for row in cases {
        let case = row.case;
        store_table(&router, database, row.table, row.columns).await;
        // The table has no run of its own before the create, so the one below is the
        // create's.
        let before = runs_of(&router, database, row.table).await;
        assert!(
            before.is_empty(),
            "case {case:?}: a table nothing has run yet has an empty history, got {before:?}"
        );

        ok(
            &router,
            "CreateColumnStatisticsTaskSettings",
            &create_body(database, row.table, row.extra.clone()),
        )
        .await;
        // The window the clock owes the started run is held open THROUGH THE STORE, so the
        // assertions below do not race the host.
        set_due_instant(&engine, far_future());

        let run = the_only_run_of(&router, case, database, row.table).await;
        assert_in_flight(case, &run);
        assert_eq!(
            run.get("Role").and_then(Value::as_str),
            Some(row.expected_role),
            "case {case:?}: the run reports the role the create stated, got {run}"
        );
        assert_eq!(
            reported_columns(&run),
            row.expected_columns
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the run to cover {:?}, got {run}",
            row.expected_columns
        );
        assert_eq!(
            reported_number(&run, "SampleSize"),
            row.expected_sample,
            "case {case:?}: expected SampleSize {}, got {run}",
            row.expected_sample
        );
        assert_eq!(
            run.get("SecurityConfiguration").and_then(Value::as_str),
            row.expected_security_configuration,
            "case {case:?}: expected the security configuration {:?}, got {run}",
            row.expected_security_configuration
        );
        assert_eq!(
            run.get("DatabaseName").and_then(Value::as_str),
            Some(database),
            "case {case:?}: the run covers the table the create addressed, got {run}"
        );
        assert_eq!(
            run.get("TableName").and_then(Value::as_str),
            Some(row.table),
            "case {case:?}: the run covers the table the create addressed, got {run}"
        );
        assert_eq!(
            run.get("CatalogID").and_then(Value::as_str),
            Some(ACCOUNT_ID),
            "case {case:?}: the run belongs to the catalog the create resolved in, got {run}"
        );

        // The settings and the run are the same table's: the entry the create wrote reports
        // the run it started as the table's last attempt.
        let settings = get_settings(&router, &read_body(database, row.table)).await;
        assert_eq!(
            settings
                .get("LastExecutionAttempt")
                .and_then(|attempt| attempt.get("ColumnStatisticsTaskRunId"))
                .and_then(Value::as_str),
            run.get("ColumnStatisticsTaskRunId").and_then(Value::as_str),
            "case {case:?}: the settings report the run their create started, got {settings}"
        );

        // The table is left idle for the rows that follow, which is what the clock owed the
        // run anyway.
        set_due_instant(&engine, already_past());
    }
}

/// A create that states a `Schedule` starts no run — the schedule is when its runs are due
/// — so the listing of that table stays empty until something starts one.
#[tokio::test]
async fn a_create_that_states_a_schedule_starts_no_run() {
    let (router, engine) = emulator_with_engine();
    let database = "scheduled_db";

    // (case, the cron expression the create states) — the documented example and a second
    // expression of the same documented form.
    for (case, schedule) in [
        ("the documented daily expression", DAILY_SCHEDULE),
        ("an hourly expression", "cron(0 * * * ? *)"),
    ] {
        let table = schedule
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>();
        store_table(&router, database, &table, &["id"]).await;
        ok(
            &router,
            "CreateColumnStatisticsTaskSettings",
            &create_body(
                database,
                &table,
                json!({ "Role": ROLE, "Schedule": schedule }),
            ),
        )
        .await;

        let runs = runs_of(&router, database, &table).await;
        assert!(
            runs.is_empty(),
            "case {case:?}: a create that states a schedule starts no run, got {runs:?}"
        );

        // The empty listing is this table having no run and not a listing that reports
        // none: an explicit start of the same table is reported at once.
        ok(
            &router,
            "StartColumnStatisticsTaskRun",
            &json!({ "DatabaseName": database, "TableName": table, "Role": ROLE }),
        )
        .await;
        set_due_instant(&engine, far_future());
        let run = the_only_run_of(&router, case, database, &table).await;
        assert_in_flight(case, &run);
        set_due_instant(&engine, already_past());
    }
}

// ---------------------------------------------------------------------------
// The conflict that run creates, and the stop that ends it
// ---------------------------------------------------------------------------

/// While the run a schedule-less create started is in flight, a second create for that
/// table is the modelled in-flight refusal and changes nothing — not the stored settings and
/// not the single run; and once a stop has ended that run, the create is served and starts a
/// run of its own, carrying the role IT stated.
#[tokio::test]
async fn the_started_run_refuses_a_second_create_until_a_stop_ends_it() {
    let (router, engine) = emulator_with_engine();
    let database = "conflict_db";
    let table = "conflict_table";
    store_table(&router, database, table, &["id", "amount"]).await;

    ok(
        &router,
        "CreateColumnStatisticsTaskSettings",
        &create_body(database, table, json!({ "Role": ROLE })),
    )
    .await;
    set_due_instant(&engine, far_future());
    let first_run = the_only_run_of(&router, "the create's own run", database, table).await;
    assert_in_flight("the create's own run", &first_run);
    let first_run_id = first_run
        .get("ColumnStatisticsTaskRunId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("every reported run carries its id, got {first_run}"))
        .to_string();

    // (case, what the second create states beyond the addressed table) — the rows differ
    // along what that create would reconfigure: another role, another role with another
    // column list or sample size, and the same role again.
    let refused: Vec<(&str, Value)> = vec![
        ("another role", json!({ "Role": OTHER_ROLE })),
        (
            "another role and another column list",
            json!({ "Role": OTHER_ROLE, "ColumnNameList": ["amount"] }),
        ),
        (
            "another role and another sample size",
            json!({ "Role": OTHER_ROLE, "SampleSize": 25.0 }),
        ),
        ("the same role again", json!({ "Role": ROLE })),
    ];

    for (case, extra) in refused {
        let (status, parsed) = call(
            &router,
            "CreateColumnStatisticsTaskSettings",
            &create_body(database, table, extra.clone()),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a create under the run the first one started is refused, got {status} \
             with {parsed}"
        );
        assert!(
            error_code(&parsed).ends_with("ColumnStatisticsTaskRunningException"),
            "case {case:?}: expected ColumnStatisticsTaskRunningException, got {} with {}",
            error_code(&parsed),
            error_message(&parsed)
        );
        assert_eq!(
            error_message(&parsed),
            COLUMN_STATISTICS_TASK_SETTINGS_RUN_ALREADY_STARTED,
            "case {case:?}: the refusal carries this error class's fixed message, got {:?}",
            error_message(&parsed)
        );

        // The refusal had no effect: the one run is still the first create's, and the
        // stored settings are still the ones that started it.
        let run = the_only_run_of(&router, case, database, table).await;
        assert_eq!(
            run.get("ColumnStatisticsTaskRunId").and_then(Value::as_str),
            Some(first_run_id.as_str()),
            "case {case:?}: the refused create started no run of its own, got {run}"
        );
        assert_eq!(
            run.get("Role").and_then(Value::as_str),
            Some(ROLE),
            "case {case:?}: the run in flight is still the first create's, got {run}"
        );
        assert_in_flight(case, &run);
        let settings = get_settings(&router, &read_body(database, table)).await;
        assert_eq!(
            settings.get("Role").and_then(Value::as_str),
            Some(ROLE),
            "case {case:?}: a refused create rewrites nothing, got {settings}"
        );
    }

    // A stop ends the run, so the conflict goes with it.
    ok(
        &router,
        "StopColumnStatisticsTaskRun",
        &json!({ "DatabaseName": database, "TableName": table }),
    )
    .await;
    let stopped = the_only_run_of(&router, "after the stop", database, table).await;
    assert_eq!(
        status_of(&stopped),
        COLUMN_STATISTICS_TASK_RUN_STATE_STOPPED,
        "case {:?}: a stopped run reads back in the stopped state, got {stopped}",
        "after the stop"
    );

    // The create that was refused is now served, and starts a run of its own carrying the
    // role IT stated.
    ok(
        &router,
        "CreateColumnStatisticsTaskSettings",
        &create_body(database, table, json!({ "Role": OTHER_ROLE })),
    )
    .await;
    set_due_instant(&engine, far_future());
    let runs = runs_of(&router, database, table).await;
    assert_eq!(
        runs.len(),
        2,
        "case {:?}: the served create started a second run beside the stopped one, got {runs:?}",
        "after the stop"
    );
    let newest = runs[0].clone();
    assert_in_flight("the second create's run", &newest);
    assert_eq!(
        newest.get("Role").and_then(Value::as_str),
        Some(OTHER_ROLE),
        "case {:?}: the new run reports the role the served create stated, got {newest}",
        "the second create's run"
    );
    assert_ne!(
        newest.get("ColumnStatisticsTaskRunId").and_then(Value::as_str),
        Some(first_run_id.as_str()),
        "case {:?}: each create mints its own run, got {newest}",
        "the second create's run"
    );
    assert_eq!(
        runs[1].get("ColumnStatisticsTaskRunId").and_then(Value::as_str),
        Some(first_run_id.as_str()),
        "case {:?}: the stopped run is still part of the table's history, got {:?}",
        "after the stop",
        runs[1]
    );
    let settings = get_settings(&router, &read_body(database, table)).await;
    assert_eq!(
        settings.get("Role").and_then(Value::as_str),
        Some(OTHER_ROLE),
        "case {:?}: the served create reconfigured the settings, got {settings}",
        "after the stop"
    );
}

// ---------------------------------------------------------------------------
// The one wording every miss on the settings sub-resource answers
// ---------------------------------------------------------------------------

/// One row of the miss table below: an address the catalog holds no settings entry under.
struct MissCase {
    /// How this row's address misses, named in each of its assertions.
    case: &'static str,
    database: String,
    table: String,
    /// The `CatalogID` the request names, where it names one.
    catalog_id: Option<&'static str>,
    /// Whether the refusal must be checked for carrying neither name: a row whose names are
    /// a single letter cannot be, because the fixed sentence spells words out of letters.
    withholds_names: bool,
}

impl MissCase {
    /// The body addressing this row's entry, which both operations take the same way.
    fn body(&self) -> Value {
        let mut body = json!({ "DatabaseName": self.database, "TableName": self.table });
        if let Some(catalog_id) = self.catalog_id {
            body["CatalogID"] = json!(catalog_id);
        }
        body
    }
}

/// Both operations that address the settings of a table — the read and the delete — answer
/// the SAME miss, in the same fixed wording, whichever half of the (database, table) pair
/// the catalog holds nothing under: the entry the pair addresses is one sub-resource, so the
/// refusal is about that entry and names neither of the two names that addressed it.
#[tokio::test]
async fn every_miss_on_the_settings_of_a_table_answers_one_fixed_sentence() {
    let router = emulator_on(Arc::new(engine::InMemoryEngine::new()));
    store_table(&router, "miss_db", "bare_table", &["id"]).await;
    store_table(&router, "miss_db", "kept_table", &["id"]).await;
    store_table(&router, "other_miss_db", "elsewhere_table", &["id"]).await;
    // One entry that DOES exist, so each refusal below is checked against a catalog that
    // serves the address it holds an entry for.
    ok(
        &router,
        "CreateColumnStatisticsTaskSettings",
        &create_body(
            "miss_db",
            "kept_table",
            json!({ "Role": ROLE, "Schedule": DAILY_SCHEDULE }),
        ),
    )
    .await;

    // The rows differ along HOW the address misses: a stored table carrying no entry, a
    // table the database does not hold, the name of a table another database holds, a
    // database the catalog does not hold, a pair naming NEITHER a stored database nor a
    // stored table, the catalog of another account, and the shortest and longest names
    // `NameString` admits (`length 1..255`,
    // `smithy_query operation=DeleteColumnStatisticsTaskSettings`).
    let cases: Vec<MissCase> = vec![
        MissCase {
            case: "a stored table carrying no entry",
            database: "miss_db".to_string(),
            table: "bare_table".to_string(),
            catalog_id: None,
            withholds_names: true,
        },
        MissCase {
            case: "a table the database does not hold",
            database: "miss_db".to_string(),
            table: "absent_table".to_string(),
            catalog_id: None,
            withholds_names: true,
        },
        MissCase {
            case: "the name of a table another database holds",
            database: "miss_db".to_string(),
            table: "elsewhere_table".to_string(),
            catalog_id: None,
            withholds_names: true,
        },
        MissCase {
            case: "a database the catalog does not hold",
            database: "absent_db".to_string(),
            table: "kept_table".to_string(),
            catalog_id: None,
            withholds_names: true,
        },
        MissCase {
            // The recorded case: a pair naming nothing the catalog holds at all.
            case: "a pair naming neither a stored database nor a stored table",
            database: "absent_db".to_string(),
            table: "absent_table".to_string(),
            catalog_id: None,
            withholds_names: true,
        },
        MissCase {
            case: "the catalog of another account",
            database: "miss_db".to_string(),
            table: "kept_table".to_string(),
            catalog_id: Some("210987654321"),
            withholds_names: true,
        },
        MissCase {
            case: "the shortest names the model admits",
            database: "d".to_string(),
            table: "t".to_string(),
            catalog_id: None,
            withholds_names: false,
        },
        MissCase {
            case: "the longest names the model admits",
            database: "d".repeat(255),
            table: "t".repeat(255),
            catalog_id: None,
            withholds_names: true,
        },
    ];

    for row in cases {
        let body = row.body();
        for operation in [
            "GetColumnStatisticsTaskSettings",
            "DeleteColumnStatisticsTaskSettings",
        ] {
            let case = format!("{} on {}", row.case, operation);
            let (status, parsed) = call(&router, operation, &body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: an address holding no settings entry is refused, got {status} \
                 with {parsed}"
            );
            assert!(
                error_code(&parsed).ends_with("EntityNotFoundException"),
                "case {case:?}: expected EntityNotFoundException, got {}",
                error_code(&parsed)
            );
            assert_eq!(
                error_message(&parsed),
                COLUMN_STATISTICS_TASK_SETTINGS_NOT_FOUND,
                "case {case:?}: expected the entry's own fixed sentence, got {:?}",
                error_message(&parsed)
            );
            if row.withholds_names {
                let message = error_message(&parsed);
                assert!(
                    !message.contains(&row.database) && !message.contains(&row.table),
                    "case {case:?}: the refusal names neither half of the address it was sent, \
                     got {message:?}"
                );
            }
        }

        // Neither refusal touched the one entry the catalog does hold.
        let settings = get_settings(&router, &read_body("miss_db", "kept_table")).await;
        assert_eq!(
            settings.get("Role").and_then(Value::as_str),
            Some(ROLE),
            "case {:?}: a refused read or delete removes nothing, got {settings}",
            row.case
        );
    }

    // And the entry a delete DOES address is removed, so the refusals above are misses and
    // not a broken lookup.
    ok(
        &router,
        "DeleteColumnStatisticsTaskSettings",
        &read_body("miss_db", "kept_table"),
    )
    .await;
    let (status, parsed) = call(
        &router,
        "GetColumnStatisticsTaskSettings",
        &read_body("miss_db", "kept_table"),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {:?}: the deleted entry is gone, got {status} with {parsed}",
        "the stored entry after every refused call"
    );
    assert_eq!(
        error_message(&parsed),
        COLUMN_STATISTICS_TASK_SETTINGS_NOT_FOUND,
        "case {:?}: expected the entry's own fixed sentence, got {:?}",
        "the stored entry after every refused call",
        error_message(&parsed)
    );
}
