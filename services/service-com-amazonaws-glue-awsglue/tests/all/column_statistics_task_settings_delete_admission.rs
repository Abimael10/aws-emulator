// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what `DeleteColumnStatisticsTaskSettings` answers when the pair it
//! addresses carries no settings entry BECAUSE the create that would have written one was
//! refused.
//!
//! The rule under test: the column statistics task settings of a table come into being only
//! where an ADMITTED `CreateColumnStatisticsTaskSettings` wrote them, so a delete addressed
//! at a (database, table) pair that carries no entry removes nothing and is refused with the
//! operation's modelled `EntityNotFoundException` (HTTP 400) in its own fixed sentence —
//! never a 200 — however that absence came about: no create was ever made for the pair, the
//! create was refused (for the role it named, for the table or database it addressed, or for
//! the schedule it stated), or an earlier delete already removed the entry. A 200 therefore
//! means an entry really was removed, which is exactly what a client cannot learn from a
//! delete that reports success either way.
//!
//! The rows of each table below differ along WHY the pair carries no entry and along the form
//! the delete addresses it in (the stored casing, another casing, with and without an explicit
//! `CatalogID`); the positive control holds the same addresses over an entry an admitted
//! create wrote, so the refusal is pinned as a decision about the entry and not a blanket
//! refusal of the operation.
//!
//! The role resolution the refused creates here turn on needs IAM mounted to answer at all,
//! so these suites stand up the emulator the role-admission suites do
//! ([`crate::crawler_role_trust::emulator_with_iam`]). Every call goes over the wire through
//! the shared [`crate::wire`] harness, so what is asserted is HTTP behaviour and not a direct
//! call into a handler.

use serde_json::{json, Value};

use crate::column_statistics_task_settings::{
    create_body, get_settings, read_body, store_table, DAILY_SCHEDULE,
};
// The addresses, the refusal assertions and the fixed sentence are the deletion suite's: one
// copy drives both, so the delete this suite drives is the delete that suite deletes with.
use crate::column_statistics_task_settings_deletion::{
    assert_delete_is_miss, assert_read_is_miss, delete_body, delete_settings,
    SETTINGS_MISS_MESSAGE,
};
use crate::crawler_role_trust::{emulator_with_iam, glue_trust, seed_role, trust};
use crate::wire::{call, ok, ACCOUNT_ID};

/// The database every row addresses its table in.
const DATABASE: &str = "delete_admission_db";

/// One row of the refusal table: the create that must be refused, if the row makes one, and
/// the delete that must then report the modelled miss.
struct Row<'a> {
    /// What this row varies, named in each of its assertions.
    case: &'a str,
    /// The create this row attempts, which must be refused — or `None` for a pair no create
    /// ever addressed.
    create: Option<Value>,
    /// The database the delete addresses the entry by.
    database: &'a str,
    /// The table the delete addresses the entry by.
    table: &'a str,
    /// The delete members beyond the two `@required` ones.
    delete_extra: Value,
}

/// A pair whose create was refused carries no settings entry, so the delete of that pair
/// removes nothing and reports the operation's own modelled miss — the same refusal, in the
/// same fixed sentence, as a pair no create ever addressed, and it stays that way when the
/// delete is repeated.
#[tokio::test]
async fn a_delete_is_the_modelled_miss_wherever_a_refused_create_left_no_entry() {
    let (router, engine) = emulator_with_iam();

    // The roles the creates below name: one Glue can assume, and three it cannot — a trust
    // document admitting another service, a role carrying no trust document, and a role name
    // this account holds nothing under.
    let assumable = seed_role(&engine, "delete-admission-assumable", Some(glue_trust()));
    let other_service = seed_role(
        &engine,
        "delete-admission-other-service",
        Some(trust("Allow", "lambda.amazonaws.com", "sts:AssumeRole")),
    );
    let no_trust_document = seed_role(&engine, "delete-admission-no-trust", None);
    let not_held = "delete-admission-not-held";

    for table in [
        "other_service_role_table",
        "no_trust_document_table",
        "unheld_role_table",
        "bad_schedule_table",
        "never_created_table",
    ] {
        store_table(&router, DATABASE, table, &["a"]).await;
    }

    let rows: Vec<Row<'_>> = vec![
        Row {
            case: "a create refused because the role's trust document admits another service",
            create: Some(create_body(
                DATABASE,
                "other_service_role_table",
                json!({ "Role": other_service, "Schedule": DAILY_SCHEDULE }),
            )),
            database: DATABASE,
            table: "other_service_role_table",
            delete_extra: json!({}),
        },
        Row {
            case: "a create refused because the role carries no trust document",
            create: Some(create_body(
                DATABASE,
                "no_trust_document_table",
                json!({ "Role": no_trust_document }),
            )),
            database: DATABASE,
            table: "no_trust_document_table",
            delete_extra: json!({}),
        },
        Row {
            case: "a create refused because the account holds no such role",
            create: Some(create_body(
                DATABASE,
                "unheld_role_table",
                json!({ "Role": not_held }),
            )),
            database: DATABASE,
            table: "unheld_role_table",
            delete_extra: json!({}),
        },
        Row {
            case: "a create refused because the catalog holds no such table",
            create: Some(create_body(
                DATABASE,
                "no_such_table",
                json!({ "Role": assumable.clone(), "Schedule": DAILY_SCHEDULE }),
            )),
            database: DATABASE,
            table: "no_such_table",
            delete_extra: json!({}),
        },
        Row {
            case: "a create refused because the catalog holds no such database",
            create: Some(create_body(
                "no_such_delete_admission_db",
                "never_created_table",
                json!({ "Role": assumable.clone() }),
            )),
            database: "no_such_delete_admission_db",
            table: "never_created_table",
            delete_extra: json!({}),
        },
        // `Schedule` is "a schedule for running the column statistics, specified in CRON
        // syntax" (`smithy_query operation=CreateColumnStatisticsTaskSettings`), so a value
        // that is not that expression is a create the service refuses as well.
        Row {
            case: "a create refused because the schedule is not a cron expression",
            create: Some(create_body(
                DATABASE,
                "bad_schedule_table",
                json!({ "Role": assumable.clone(), "Schedule": "every day at noon" }),
            )),
            database: DATABASE,
            table: "bad_schedule_table",
            delete_extra: json!({}),
        },
        Row {
            case: "a stored pair no create ever addressed",
            create: None,
            database: DATABASE,
            table: "never_created_table",
            delete_extra: json!({}),
        },
        Row {
            case: "a stored pair no create ever addressed, named in another casing",
            create: None,
            database: "Delete_Admission_DB",
            table: "Never_Created_Table",
            delete_extra: json!({}),
        },
        Row {
            case: "a stored pair no create ever addressed, in the catalog named explicitly",
            create: None,
            database: DATABASE,
            table: "never_created_table",
            delete_extra: json!({ "CatalogID": ACCOUNT_ID }),
        },
    ];

    for row in rows {
        let case = row.case;
        if let Some(create) = &row.create {
            let (status, parsed) = call(&router, "CreateColumnStatisticsTaskSettings", create).await;
            assert_eq!(
                status, 400,
                "case {case:?}: the create this row attempts must be refused, got {status} with \
                 {parsed}"
            );
        }

        // No entry came into being, so the read reports the miss and the delete refuses
        // instead of reporting a success that removed nothing.
        assert_read_is_miss(
            &router,
            case,
            &read_body(row.database, row.table),
            SETTINGS_MISS_MESSAGE,
        )
        .await;
        let body = delete_body(row.database, row.table, row.delete_extra.clone());
        assert_delete_is_miss(&router, case, &body, SETTINGS_MISS_MESSAGE).await;
        // Repeating it changes nothing: the refusal is a reading of the store, not a state
        // the first delete consumed.
        assert_delete_is_miss(
            &router,
            &format!("{case}, deleted a second time"),
            &body,
            SETTINGS_MISS_MESSAGE,
        )
        .await;
    }
}

/// The positive control over the same addresses: where the create IS admitted, the entry it
/// wrote reads back, the first delete removes it and answers 200, and only the delete after
/// it reports the modelled miss — so the refusals above are a decision about the entry rather
/// than a refusal of the operation or of these addresses.
#[tokio::test]
async fn a_delete_answers_the_empty_success_only_while_an_admitted_create_left_an_entry() {
    let (router, engine) = emulator_with_iam();
    let assumable = seed_role(&engine, "delete-admission-admitted", Some(glue_trust()));

    // (case, the table, the address the delete uses) — the rows differ along the form the
    // delete addresses the entry in, each of which must reach the entry the create wrote.
    let rows: Vec<(&str, &str, &str, &str, Value)> = vec![
        (
            "the stored casing of both names",
            "admitted_table",
            DATABASE,
            "admitted_table",
            json!({}),
        ),
        (
            "the names in another casing",
            "admitted_mixed_table",
            "Delete_Admission_DB",
            "Admitted_Mixed_Table",
            json!({}),
        ),
        (
            "the account's own catalog named explicitly",
            "admitted_catalog_table",
            DATABASE,
            "admitted_catalog_table",
            json!({ "CatalogID": ACCOUNT_ID }),
        ),
    ];

    for (case, table, delete_database, delete_table, delete_extra) in rows {
        store_table(&router, DATABASE, table, &["a"]).await;
        // A create stating a schedule is a standing task, so it starts no run of its own and
        // the delete below is the only thing that changes the entry.
        ok(
            &router,
            "CreateColumnStatisticsTaskSettings",
            &create_body(
                DATABASE,
                table,
                json!({ "Role": assumable.clone(), "Schedule": DAILY_SCHEDULE }),
            ),
        )
        .await;
        let reported = get_settings(&router, &read_body(DATABASE, table)).await;
        assert_eq!(
            reported.get("Role").and_then(Value::as_str),
            Some(assumable.as_str()),
            "case {case:?}: the admitted create's settings must read back, got {reported}"
        );

        let body = delete_body(delete_database, delete_table, delete_extra);
        delete_settings(&router, &body).await;
        assert_read_is_miss(
            &router,
            case,
            &read_body(DATABASE, table),
            SETTINGS_MISS_MESSAGE,
        )
        .await;
        assert_delete_is_miss(&router, case, &body, SETTINGS_MISS_MESSAGE).await;
    }
}
