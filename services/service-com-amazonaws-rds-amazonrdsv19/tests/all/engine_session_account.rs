// SPDX-License-Identifier: CC0-1.0

//! The SESSION ACCOUNT a MySQL-compatible DB instance endpoint reports and resolves.
//!
//! The rules under test, each asserted over a table of inputs rather than one recorded
//! statement:
//!
//! * wherever MySQL's grammar takes an ACCOUNT, the keyword `CURRENT_USER` — written with or
//!   without an empty argument list, in any case — is EVALUATED to the account the session
//!   holds the privileges of, so `SHOW GRANTS FOR CURRENT_USER()` is `SHOW GRANTS` and a
//!   `GRANT … TO CURRENT_USER()` grants to the session's own account. Quoted, the same
//!   spelling is a user of that name, which this server holds no grant for.
//! * in an EXPRESSION, `CURRENT_USER`/`CURRENT_USER()` evaluate to that account as a string
//!   value (`<user>@<host-pattern>`), and `USER()`/`SESSION_USER()` to the user name with the
//!   host the client reached the endpoint on.
//! * the master user's list holds no `SUPER`, and a statement that would need it is refused
//!   naming the privileges the engine FAMILY defines for it.
//!
//! The rows differ along what each rule generalises over: the engine family of the instance
//! (MySQL and MariaDB both take this grammar), how the keyword is written, which ACCOUNT the
//! session authenticated as (the RDS master user, and a database user created on the instance
//! and logged in with an authentication token), and which statement class names the account.

use std::time::SystemTime;

use crate::db_instance_engine_endpoint::{
    available_instance, mysql_login, Refusal, Transport, DATABASE, MASTER_USER,
};
use crate::db_instances::{emulator, REGION};
// The MySQL-wire helpers of the suite that already drives this endpoint's account statements:
// one copy of each, shared rather than written again.
use crate::engine_iam_authentication_mysql::{grants, master_session, run, value, IAM_USER};
use crate::engine_role_grants::auth_token;

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// Every way of naming the session's own account in the `SHOW GRANTS` grammar: the statement
/// that names none, and the keyword in the spellings MySQL's `user` value takes it in.
const THE_SESSIONS_OWN_ACCOUNT: [(&str, &str); 5] = [
    ("naming no account at all", "SHOW GRANTS"),
    ("the keyword", "SHOW GRANTS FOR CURRENT_USER"),
    (
        "the keyword with an empty argument list",
        "SHOW GRANTS FOR CURRENT_USER()",
    ),
    (
        "the keyword written lowercase",
        "SHOW GRANTS FOR current_user()",
    ),
    (
        "the keyword written in mixed case, spaced from its argument list",
        "SHOW GRANTS FOR Current_User ()",
    ),
];

/// RULE: on every MySQL-compatible engine family, the account-valued keyword in the account
/// grammar is the session's own account — so the master user reads its own privilege list back
/// through it, while the same spelling QUOTED is a user of that name the server holds no grant
/// for. The list holds no `SUPER`, and the statement that would need it is refused naming the
/// privileges THAT family defines for it.
#[tokio::test]
async fn the_account_keyword_and_the_privilege_refusal_follow_the_engine_family() {
    let router = emulator();
    // The two engine families whose endpoints speak this grammar; the rows differ in the
    // family, which is what the rule generalises over.
    for (case, engine, identifier, privileges) in [
        (
            "MySQL",
            "mysql",
            "emu-session-account-mysql",
            "SUPER or SYSTEM_VARIABLES_ADMIN",
        ),
        (
            "MariaDB",
            "mariadb",
            "emu-session-account-mariadb",
            "SUPER",
        ),
    ] {
        let (address, port) = available_instance(&router, identifier, engine, &params![]).await;
        let mut session = master_session(&address, port).await;
        // The master user's own privilege list, read through the account written out.
        let written = grants(
            case,
            &mut session,
            &format!("SHOW GRANTS FOR '{MASTER_USER}'@'%'"),
        )
        .await;
        assert!(
            written
                .iter()
                .any(|statement| statement
                    .ends_with(&format!("TO `{MASTER_USER}`@`%` WITH GRANT OPTION"))),
            "case {case:?}: the account written out must report the master user's grants, got \
             {written:?}"
        );
        assert!(
            !written.iter().any(|statement| statement.contains("SUPER")),
            "case {case:?}: the RDS master user holds no SUPER privilege, got {written:?}"
        );
        for (form, statement) in THE_SESSIONS_OWN_ACCOUNT {
            let reported = grants(case, &mut session, statement).await;
            assert_eq!(
                reported, written,
                "case {case:?} / {form:?}: {statement:?} must report the same grants as the \
                 account written out ({written:?}), got {reported:?}"
            );
        }
        // Quoted, the same spelling is a user NAME: no account of it exists, which is the
        // condition MySQL numbers for an account with no grants.
        let quoted = session
            .query("SHOW GRANTS FOR 'current_user'")
            .await
            .err()
            .unwrap_or_else(|| {
                panic!("case {case:?}: a quoted user name nothing created must be refused")
            });
        assert_eq!(
            quoted,
            Refusal {
                code: "1141".to_string(),
                message: "There is no such grant defined for user 'current_user' on host '%'"
                    .to_string(),
            },
            "case {case:?}: expected MySQL's own no-such-grant refusal, got {quoted:?}"
        );
        // What the absent `SUPER` means for the statement that would need it: the refusal
        // names the privileges this family defines for changing a global system variable, in
        // every scope spelling that names the server rather than the session.
        for (form, statement) in [
            ("a global write", "SET GLOBAL sort_buffer_size = 1048576"),
            ("a persisted write", "SET PERSIST sort_buffer_size = 1048576"),
            (
                "a global write in the system-variable form",
                "SET @@GLOBAL.sort_buffer_size = 1048576",
            ),
            (
                "a persisted write in the system-variable form",
                "SET @@persist.sort_buffer_size = 1048576",
            ),
        ] {
            let refused = session.query(statement).await.err().unwrap_or_else(|| {
                panic!("case {case:?} / {form:?}: {statement:?} must be refused")
            });
            let expected = Refusal {
                code: "1227".to_string(),
                message: format!(
                    "Access denied; you need (at least one of) the {privileges} privilege(s) \
                     for this operation"
                ),
            };
            assert_eq!(
                refused, expected,
                "case {case:?} / {form:?}: expected {expected:?}, got {refused:?}"
            );
        }
        // The same variable in the SESSION's own scope is the session's to set: what the
        // refusal above turns on is the scope, not the variable or the statement.
        for (form, statement) in [
            ("the session's own scope, unqualified", "SET sort_buffer_size = 1048576"),
            (
                "the session's own scope, in the system-variable form",
                "SET @@session.sort_buffer_size = 1048576",
            ),
        ] {
            run(&format!("{case} / {form}"), &mut session, statement).await;
        }
    }
}

/// RULE: the account the keyword stands for is the account THIS session authenticated as, and
/// a statement that names it writes the catalog of that account — so two sessions on one
/// endpoint read and grant to two different accounts through the same statement text. And in
/// an expression the same identity is a string VALUE, not a name the engine resolves.
#[tokio::test]
async fn the_session_account_is_the_account_this_session_authenticated_as() {
    let router = emulator();
    let (address, port) = available_instance(
        &router,
        "emu-session-account-iam",
        "mysql",
        &params!["EnableIAMDatabaseAuthentication" => "true"],
    )
    .await;
    let mut session = master_session(&address, port).await;
    run(
        "the IAM account",
        &mut session,
        &format!("CREATE USER '{IAM_USER}'@'%' IDENTIFIED WITH AWSAuthenticationPlugin AS 'RDS'"),
    )
    .await;
    run(
        "the grant",
        &mut session,
        &format!("GRANT SELECT ON *.* TO '{IAM_USER}'@'%'"),
    )
    .await;
    let token = auth_token(&address, port, IAM_USER, REGION, SystemTime::now(), 900);
    let mut token_session = mysql_login(
        &address,
        port,
        Transport::Tls,
        IAM_USER,
        &token,
        Some(DATABASE),
    )
    .await
    .expect("an account enabled for IAM authentication must be admitted by its token");

    // The same statement text, on two sessions: each reports the privileges of the account
    // that session holds, and the token-authenticated one is the account the token named.
    let on_the_token_session = grants(
        "the token-authenticated session",
        &mut token_session,
        "SHOW GRANTS FOR CURRENT_USER()",
    )
    .await;
    assert_eq!(
        on_the_token_session,
        vec![
            format!("GRANT USAGE ON *.* TO `{IAM_USER}`@`%`"),
            format!("GRANT SELECT ON *.* TO `{IAM_USER}`@`%`"),
        ],
        "case \"the token-authenticated session\": expected the grants of the account the token \
         named, got {on_the_token_session:?}"
    );
    let on_the_master_session = grants(
        "the master session",
        &mut session,
        "SHOW GRANTS FOR CURRENT_USER()",
    )
    .await;
    assert!(
        on_the_master_session
            .iter()
            .all(|statement| !statement.contains(IAM_USER)),
        "case \"the master session\": the keyword must stand for the master user's own account, \
         got {on_the_master_session:?}"
    );

    // The keyword in a statement that WRITES the catalog: what it granted is what the same
    // account reads back.
    run(
        "a grant to the session's own account",
        &mut token_session,
        &format!("GRANT INSERT ON {DATABASE}.* TO CURRENT_USER()"),
    )
    .await;
    let after = grants(
        "a grant to the session's own account",
        &mut token_session,
        "SHOW GRANTS FOR CURRENT_USER()",
    )
    .await;
    assert_eq!(
        after,
        vec![
            format!("GRANT USAGE ON *.* TO `{IAM_USER}`@`%`"),
            format!("GRANT SELECT ON *.* TO `{IAM_USER}`@`%`"),
            format!("GRANT INSERT ON {DATABASE}.* TO `{IAM_USER}`@`%`"),
        ],
        "case \"a grant to the session's own account\": the grant must have been recorded for \
         the session's own account, got {after:?}"
    );

    // In an EXPRESSION the identity is a value: the account for the two forms that report it,
    // and the user with the host the client connected on for the two that report that.
    for (case, account_of, user_of) in [
        ("the master session", MASTER_USER, MASTER_USER),
        ("the token-authenticated session", IAM_USER, IAM_USER),
    ] {
        let opened = match case {
            "the master session" => &mut session,
            _ => &mut token_session,
        };
        for (form, statement, expected) in [
            (
                "the keyword, called",
                "SELECT CURRENT_USER()",
                format!("{account_of}@%"),
            ),
            (
                "the keyword, written without an argument list",
                "SELECT CURRENT_USER",
                format!("{account_of}@%"),
            ),
            (
                "the connected user",
                "SELECT USER()",
                format!("{user_of}@{address}"),
            ),
            (
                "the session user",
                "SELECT SESSION_USER()",
                format!("{user_of}@{address}"),
            ),
        ] {
            let read = value(case, opened, statement).await;
            assert_eq!(
                read.as_deref(),
                Some(expected.as_str()),
                "case {case:?} / {form:?}: expected {expected:?}, got {read:?}"
            );
        }
    }
}
