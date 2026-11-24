// SPDX-License-Identifier: CC0-1.0

//! `to_regclass` on the PostgreSQL endpoint a DB instance publishes, driven through a real
//! client session on that endpoint.
//!
//! RULE under test: **`to_regclass(text)` "translates a textual relation name to its OID",
//! and the `to_reg*` functions "return NULL rather than raising an error if the name is not
//! found" (PostgreSQL, "Object Identifier Types") — so the call answers a `regclass` for
//! every relation the session's database holds, under the name the client wrote it as, and
//! NULL for a name it holds no relation of, with the statement itself succeeding either
//! way.**
//!
//! Why it matters: a client asks the catalog whether a relation EXISTS before it reads or
//! writes it, and `to_regclass` is the question that does not fail when the answer is no.
//! A session where the function is missing cannot ask at all: the statement is refused as
//! though the relation were the problem, so a deploy check reads a healthy database as a
//! broken one.
//!
//! The rows differ along what the rule generalises over — the KIND of relation the name
//! denotes (a table, a view, one of the server's own catalogs) and the FORM the name is
//! written in (bare, schema-qualified, and qualified with a schema the database does not
//! have), beside the names that denote nothing at all.

use crate::db_instance_engine_endpoint::{available_instance, master_session, must_run};
use crate::db_instances::emulator;

/// The `pg_type` OID of `regclass`, the object identifier type that "denotes a relation"
/// (PostgreSQL, "Object Identifier Types") — the type a client decodes the column with.
const REGCLASS_TYPE_OID: i32 = 2205;

/// One row: the name the statement writes inside `to_regclass`, and the relation the call
/// must answer with — `None` for a name the database holds no relation of, which is the NULL
/// that function answers rather than an error.
struct Row {
    case: &'static str,
    written: &'static str,
    denotes: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a table of the client's own schema, named bare",
        written: "orders",
        denotes: Some("orders"),
    },
    Row {
        case: "the same table, qualified with the schema it is in",
        written: "public.orders",
        denotes: Some("orders"),
    },
    Row {
        case: "another table, so the answer follows the name and not the first relation",
        written: "shipments",
        denotes: Some("shipments"),
    },
    Row {
        case: "a VIEW, which is a relation of the catalog as much as a table is",
        written: "open_orders",
        denotes: Some("open_orders"),
    },
    Row {
        case: "the view, qualified with its schema",
        written: "public.open_orders",
        denotes: Some("open_orders"),
    },
    Row {
        case: "one of the server's own catalogs, which every database holds",
        written: "pg_class",
        denotes: Some("pg_class"),
    },
    Row {
        case: "that catalog, qualified with the schema the catalogs live in",
        written: "pg_catalog.pg_class",
        denotes: Some("pg_class"),
    },
    Row {
        case: "a name the database holds no relation of",
        written: "absent_relation",
        denotes: None,
    },
    Row {
        case: "a relation that IS there, under a schema the database does not have",
        written: "nowhere.orders",
        denotes: None,
    },
    Row {
        case: "the empty name, which denotes nothing",
        written: "",
        denotes: None,
    },
];

#[tokio::test]
async fn to_regclass_answers_the_relation_a_name_denotes_and_null_for_one_that_is_not_there() {
    let router = emulator();
    let (address, port) = available_instance(&router, "emu-regclass-pg", "postgres", &[]).await;
    let case = "the relations a name denotes";
    let mut session = master_session(case, &address, port).await;
    must_run(
        case,
        &mut session,
        "CREATE TABLE orders (id int PRIMARY KEY, total int NOT NULL)",
    )
    .await;
    must_run(
        case,
        &mut session,
        "CREATE TABLE shipments (id int PRIMARY KEY, order_id int NOT NULL)",
    )
    .await;
    must_run(
        case,
        &mut session,
        "CREATE VIEW open_orders AS SELECT id FROM orders WHERE total > 0",
    )
    .await;

    for row in ROWS {
        let case = row.case;
        let sql = format!("SELECT to_regclass('{}')", row.written);
        let read = session
            .query(&sql)
            .await
            .unwrap_or_else(|refusal| panic!("case {case:?}: {sql:?} must run, got {refusal:?}"));
        assert_eq!(
            (
                read.only(),
                read.columns.first().map(String::as_str),
                read.type_oids.first().copied(),
            ),
            (
                row.denotes,
                Some("to_regclass"),
                Some(REGCLASS_TYPE_OID),
            ),
            "case {case:?}: expected {:?} under the regclass type on a column named after the \
             function, got {read:?}",
            row.denotes
        );
    }

    // The question a client really asks with it: whether the relation is there — answered for
    // one the database holds and for one it does not, rather than refused for either.
    for (name, state) in [("orders", "present"), ("absent_relation", "absent")] {
        let sql = format!(
            "SELECT CASE WHEN to_regclass('{name}') IS NULL THEN 'absent' ELSE 'present' END \
             AS state"
        );
        let read = session
            .query(&sql)
            .await
            .unwrap_or_else(|refusal| panic!("case {name:?}: {sql:?} must run, got {refusal:?}"));
        assert_eq!(
            read.only(),
            Some(state),
            "case {name:?}: expected the existence test to answer {state:?}, got {read:?}"
        );
    }

    // A relation that is DROPPED stops being denoted, so the answer is read off the catalog as
    // it stands rather than remembered from the first call.
    must_run(case, &mut session, "DROP TABLE shipments").await;
    let dropped = session
        .query("SELECT to_regclass('shipments')")
        .await
        .expect("the call must run for a relation that has been dropped");
    assert_eq!(
        dropped.rows,
        vec![vec![None]],
        "a relation that has been dropped must be denoted by nothing, got {dropped:?}"
    );
}
