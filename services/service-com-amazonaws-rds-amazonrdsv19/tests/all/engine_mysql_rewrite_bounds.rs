// SPDX-License-Identifier: CC0-1.0

//! What the statement a MySQL session sends COSTS the endpoint that rewrites it, and what the
//! session is answered for a statement no grammar reads.
//!
//! RULE: a statement arrives from a client, so the work the server does on it before the
//! engine runs anything is bounded by the statement's own length — the rewriting that renders
//! MySQL's own spellings for the engine reads each token once — and a statement that is no
//! statement of either grammar is ANSWERED, as the refusal it is, with the session still open
//! for the next one. A pass whose cost grows faster than the statement, or that is thrown by
//! a form it did not expect, is one request wedging or severing the endpoint for every other
//! client of it.
//!
//! The rows span the forms that reach that rewriting: the auto-increment attribute with its
//! key written before it, after it and as a clause of its own (the well-formed declarations,
//! whose keys the engine must still assign), the same attribute in a statement that declares
//! nothing at all, and the shapes that combine it with the operand-taking rewrites — a cast,
//! an unmatched parenthesis, a dropped clause — which is where a pass carrying a position
//! into what it has emitted would reach past the end of it. Each row's own length is small;
//! the LENGTH axis is the long statement below, which must be answered like any other.
//!
//! Sources: the MySQL Reference Manual's "Using AUTO_INCREMENT" for the attribute and the key
//! it is declared with, and `LAST_INSERT_ID()` for the key the server reports.

use crate::db_instance_engine_endpoint::{
    available_instance, mysql_login, MySqlSession, Transport, DATABASE, MASTER_PASSWORD,
    MASTER_USER,
};
use crate::db_instances::emulator;

/// How many clauses the long statement chains: enough that a pass costing the square of the
/// statement's length would not answer, and small enough to send in one message.
const CHAINED_CLAUSES: usize = 4_000;

/// Open a session as the master user on the instance's published endpoint.
async fn session(address: &str, port: u16) -> MySqlSession {
    mysql_login(
        address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted")
}

#[tokio::test]
async fn a_statement_the_rewriting_cannot_read_is_answered_with_the_session_still_open() {
    let router = emulator();
    let (address, port) = available_instance(&router, "emu-rewrite-mysql", "mysql", &[]).await;
    let mut session = session(&address, port).await;

    // The declarations the rewriting is FOR: each one declares the engine's own counter, so
    // the server assigns the key of a row that leaves the column out — whichever way the
    // client wrote the key the attribute belongs to.
    for (case, table, declared) in [
        (
            "the key written after the attribute",
            "after_it",
            "id INT AUTO_INCREMENT PRIMARY KEY, label VARCHAR(16)",
        ),
        (
            "the key written before the attribute",
            "before_it",
            "id INT PRIMARY KEY AUTO_INCREMENT, label VARCHAR(16)",
        ),
        (
            "the key written as a clause of its own",
            "its_own_clause",
            "id INT AUTO_INCREMENT, label VARCHAR(16), PRIMARY KEY (id)",
        ),
    ] {
        session
            .query(&format!("CREATE TABLE {table} ({declared})"))
            .await
            .unwrap_or_else(|refusal| {
                panic!("case {case:?}: the declaration must be served, got {refusal:?}")
            });
        for expected in ["1", "2"] {
            session
                .query(&format!("INSERT INTO {table} (label) VALUES ('a row')"))
                .await
                .unwrap_or_else(|refusal| {
                    panic!("case {case:?}: the insert must be served, got {refusal:?}")
                });
            let reported = session
                .query("SELECT LAST_INSERT_ID()")
                .await
                .unwrap_or_else(|refusal| {
                    panic!("case {case:?}: the server reports the key, got {refusal:?}")
                });
            assert_eq!(
                reported.only(),
                Some(expected),
                "case {case:?}: the engine assigns the next key, got {reported:?}"
            );
        }
    }

    // And the statements the rewriting has to SURVIVE: each is answered — served or refused —
    // and the session is still open for the next one, which the read after the loop proves.
    let long = format!(
        "SELECT auto_increment FROM absent WHERE {}",
        std::iter::repeat_n("1=1", CHAINED_CLAUSES)
            .collect::<Vec<_>>()
            .join(" AND ")
    );
    let rows: &[(&str, &str)] = &[
        (
            "the attribute in a statement that declares nothing",
            "SELECT auto_increment FROM absent",
        ),
        (
            "the attribute after a key and an unmatched parenthesis and a cast",
            "SELECT PRIMARY KEY ) ::x AUTO_INCREMENT",
        ),
        (
            "the attribute after a cast of its own",
            "SELECT 1::int AUTO_INCREMENT",
        ),
        (
            "the attribute with a key and no definition to put it in",
            "SELECT PRIMARY KEY AUTO_INCREMENT",
        ),
        (
            "a declaration whose column list never closes",
            "CREATE TABLE unclosed (id INT AUTO_INCREMENT PRIMARY KEY",
        ),
        (
            "the attribute inside a parenthesis the statement never opens",
            "SELECT ) AUTO_INCREMENT PRIMARY KEY",
        ),
        ("the attribute alone", "AUTO_INCREMENT"),
        (
            "a long statement naming the attribute, with no comma and no parenthesis",
            long.as_str(),
        ),
    ];
    for (case, sql) in rows {
        // Whichever way the server answers it, it ANSWERS: a statement neither grammar reads
        // is the refusal it is, and this must not be the end of the session.
        let _answer = session.query(sql).await;
        let still_open = session
            .query("SELECT 1")
            .await
            .unwrap_or_else(|refusal| {
                panic!("case {case:?}: the session must still serve statements, got {refusal:?}")
            });
        assert_eq!(
            still_open.only(),
            Some("1"),
            "case {case:?}: the session answers the next statement, got {still_open:?}"
        );
    }
}
