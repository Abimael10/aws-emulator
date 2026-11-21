// SPDX-License-Identifier: CC0-1.0

//! IAM database authentication on a DB instance's MySQL endpoint: the accounts a client
//! creates, the authentication plugin each of them authenticates with, and the login an
//! authentication token admits.
//!
//! The rules under test, each asserted over a table of inputs rather than one recorded login:
//!
//! * a MySQL endpoint SERVES the account statements MySQL defines — `CREATE USER <account>
//!   IDENTIFIED WITH <plugin> [AS '<auth>']`, `GRANT <privileges> ON <scope> TO <account>`,
//!   `REVOKE`, `ALTER USER`, `DROP USER` and `FLUSH PRIVILEGES` — recording each in the
//!   catalog a MySQL server keeps it in, so a later session's `SHOW GRANTS` reports exactly
//!   what was granted, and a statement naming an account the server does not hold takes that
//!   condition's own refusal;
//! * a DB instance with `EnableIAMDatabaseAuthentication` admits a login as a database user
//!   ENABLED for it — on MySQL, an account whose authentication plugin is
//!   `AWSAuthenticationPlugin` — presenting an authentication token generated for that
//!   endpoint, that port and that user, and admits nothing else: not an account nothing
//!   enabled, not a token for another user, port, Region or moment, and not a token offered
//!   on a connection in the clear, on which the server never asks for one.
//!
//! The rows differ along what each rule generalises over: the form the account was written
//! in, the plugin it was created with, the privileges granted and the scope they were granted
//! on, every claim a token carries, and the transport the login was made over.

use std::time::{Duration, SystemTime};

use crate::db_instance_engine_endpoint::{
    available_instance, mysql_login, MySqlSession, Refusal, Transport, DATABASE, MASTER_PASSWORD,
    MASTER_USER,
};
use crate::db_instances::{emulator, REGION};
use crate::engine_role_grants::auth_token;

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The database user every IAM login below is made as.
pub(crate) const IAM_USER: &str = "iamuser";

/// Open a session as the master user on a MySQL instance's published endpoint.
pub(crate) async fn master_session(address: &str, port: u16) -> MySqlSession {
    mysql_login(
        address,
        port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted")
}

/// Run a statement that must succeed.
pub(crate) async fn run(case: &str, session: &mut MySqlSession, sql: &str) {
    if let Err(refusal) = session.query(sql).await {
        panic!("case {case:?}: {sql:?} must run, got {refusal:?}");
    }
}

/// The one value a statement answered with, or `None` for an answer with no rows.
pub(crate) async fn value(case: &str, session: &mut MySqlSession, sql: &str) -> Option<String> {
    session
        .query(sql)
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: {sql:?} must run, got {refusal:?}"))
        .only()
        .map(str::to_string)
}

/// Every row of the single column `SHOW GRANTS` answers with.
pub(crate) async fn grants(case: &str, session: &mut MySqlSession, sql: &str) -> Vec<String> {
    session
        .query(sql)
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: {sql:?} must run, got {refusal:?}"))
        .rows
        .into_iter()
        .map(|row| row.first().cloned().flatten().unwrap_or_default())
        .collect()
}

#[tokio::test]
async fn a_mysql_endpoint_serves_the_account_statements_and_records_what_was_granted() {
    let router = emulator();
    let (address, port) =
        available_instance(&router, "emu-mysql-accounts", "mysql", &params![]).await;
    let mut session = master_session(&address, port).await;

    // The forms an account is created in: quoted with a host pattern, quoted without one,
    // unquoted, and with the IAM authentication plugin.
    for (case, statement) in [
        (
            "an account with a host pattern and the IAM plugin",
            format!("CREATE USER '{IAM_USER}'@'%' IDENTIFIED WITH AWSAuthenticationPlugin AS 'RDS'"),
        ),
        (
            "an account written without a host pattern",
            "CREATE USER 'reporting' IDENTIFIED BY 'emu-Secret-77'".to_string(),
        ),
        (
            "an account written unquoted",
            "CREATE USER appuser@localhost IDENTIFIED BY 'emu-Secret-78'".to_string(),
        ),
        (
            "an account that already exists, under IF NOT EXISTS",
            format!("CREATE USER IF NOT EXISTS '{IAM_USER}'@'%'"),
        ),
    ] {
        run(case, &mut session, &statement).await;
    }

    // The privileges, on the scopes MySQL grants them on.
    for (case, statement) in [
        (
            "one privilege on every schema",
            format!("GRANT SELECT ON *.* TO '{IAM_USER}'@'%'"),
        ),
        (
            "several privileges on one schema",
            format!("GRANT SELECT, INSERT ON {DATABASE}.* TO 'reporting'@'%'"),
        ),
        (
            "a privilege on one table",
            format!("GRANT UPDATE ON {DATABASE}.widgets TO 'reporting'@'%'"),
        ),
        (
            "a privilege written in several words",
            "GRANT SHOW DATABASES ON *.* TO appuser@localhost".to_string(),
        ),
        ("re-reading the grant tables", "FLUSH PRIVILEGES".to_string()),
    ] {
        run(case, &mut session, &statement).await;
    }

    // What a later session reads back is what was granted, one statement per scope, with
    // MySQL's own `USAGE` grant for the account itself.
    for (case, account, expected) in [
        (
            "the account granted one privilege on every schema",
            format!("'{IAM_USER}'@'%'"),
            vec![
                format!("GRANT USAGE ON *.* TO `{IAM_USER}`@`%`"),
                format!("GRANT SELECT ON *.* TO `{IAM_USER}`@`%`"),
            ],
        ),
        (
            "the account granted privileges on a schema and a table",
            "'reporting'@'%'".to_string(),
            vec![
                "GRANT USAGE ON *.* TO `reporting`@`%`".to_string(),
                format!("GRANT INSERT, SELECT ON {DATABASE}.* TO `reporting`@`%`"),
                format!("GRANT UPDATE ON {DATABASE}.widgets TO `reporting`@`%`"),
            ],
        ),
        (
            "the account granted a privilege written in several words",
            "'appuser'@'localhost'".to_string(),
            vec![
                "GRANT USAGE ON *.* TO `appuser`@`localhost`".to_string(),
                "GRANT SHOW DATABASES ON *.* TO `appuser`@`localhost`".to_string(),
            ],
        ),
    ] {
        let reported = grants(case, &mut session, &format!("SHOW GRANTS FOR {account}")).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected:?}, got {reported:?}"
        );
    }

    // A privilege taken back is gone from what the account holds.
    run(
        "revoking a privilege",
        &mut session,
        &format!("REVOKE UPDATE ON {DATABASE}.widgets FROM 'reporting'@'%'"),
    )
    .await;
    let reported = grants(
        "the account a privilege was revoked from",
        &mut session,
        "SHOW GRANTS FOR 'reporting'@'%'",
    )
    .await;
    assert_eq!(
        reported,
        vec![
            "GRANT USAGE ON *.* TO `reporting`@`%`".to_string(),
            format!("GRANT INSERT, SELECT ON {DATABASE}.* TO `reporting`@`%`"),
        ],
        "case \"the account a privilege was revoked from\": expected the revoked statement to \
         be gone, got {reported:?}"
    );

    // An account dropped is an account the server no longer holds, grants and all.
    run(
        "dropping an account",
        &mut session,
        "DROP USER appuser@localhost",
    )
    .await;

    // The statements that name an account the server does not hold, each taking that
    // condition's own refusal.
    for (case, statement, expected) in [
        (
            "an account that already exists",
            format!("CREATE USER '{IAM_USER}'@'%'"),
            Refusal {
                code: "1396".to_string(),
                message: format!("Operation CREATE USER failed for '{IAM_USER}'@'%'"),
            },
        ),
        (
            "a grant to an account nothing created",
            "GRANT SELECT ON *.* TO 'nobody'@'%'".to_string(),
            Refusal {
                code: "1410".to_string(),
                message: "You are not allowed to create a user with GRANT".to_string(),
            },
        ),
        (
            "dropping an account nothing created",
            "DROP USER 'nobody'@'%'".to_string(),
            Refusal {
                code: "1396".to_string(),
                message: "Operation DROP USER failed for 'nobody'@'%'".to_string(),
            },
        ),
        (
            "the grants of an account nothing created",
            "SHOW GRANTS FOR 'nobody'@'%'".to_string(),
            Refusal {
                code: "1141".to_string(),
                message: "There is no such grant defined for user 'nobody' on host '%'"
                    .to_string(),
            },
        ),
        (
            "the grants of an account that was dropped",
            "SHOW GRANTS FOR appuser@localhost".to_string(),
            Refusal {
                code: "1141".to_string(),
                message: "There is no such grant defined for user 'appuser' on host 'localhost'"
                    .to_string(),
            },
        ),
    ] {
        let refusal = session
            .query(&statement)
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: {statement:?} must be refused"));
        assert_eq!(
            refusal, expected,
            "case {case:?}: expected {expected:?}, got {refusal:?}"
        );
    }

    // The master user is in no catalog — no statement created it — and its own grants are
    // the ones Amazon RDS grants a master user.
    let reported = grants("the master user", &mut session, "SHOW GRANTS").await;
    assert_eq!(
        reported.len(),
        1,
        "case \"the master user\": expected the one grant RDS gives it, got {reported:?}"
    );
    assert!(
        reported[0].contains(&format!("TO `{MASTER_USER}`@`%` WITH GRANT OPTION")),
        "case \"the master user\": expected its own grant statement, got {reported:?}"
    );
}

#[tokio::test]
async fn an_account_with_the_iam_plugin_authenticates_with_an_authentication_token() {
    let router = emulator();
    let (address, port) = available_instance(
        &router,
        "emu-mysql-iam-auth",
        "mysql",
        &params!["EnableIAMDatabaseAuthentication" => "true"],
    )
    .await;
    let mut session = master_session(&address, port).await;
    // The engine-side half: the account, and the authentication plugin that enables it for
    // token authentication.
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
    // An account created the same way but NOT with that plugin, to show the plugin is what
    // admits the login.
    run(
        "an account on the password plugin",
        &mut session,
        "CREATE USER 'plainuser'@'%' IDENTIFIED BY 'emu-Secret-79'",
    )
    .await;

    let now = SystemTime::now();
    let token = auth_token(&address, port, IAM_USER, REGION, now, 900);
    let mut iam_session = mysql_login(
        &address,
        port,
        Transport::Tls,
        IAM_USER,
        &token,
        Some(DATABASE),
    )
    .await
    .expect("an account enabled for IAM authentication must be admitted by its token");
    // The session is a session like any other: it runs statements, and it is the account the
    // token named.
    assert_eq!(
        value("the IAM session", &mut iam_session, "SELECT 1").await,
        Some("1".to_string()),
        "case \"the IAM session\": the admitted session must serve statements"
    );
    assert_eq!(
        value("the IAM session", &mut iam_session, "SELECT CURRENT_USER()").await,
        Some(format!("{IAM_USER}@%")),
        "case \"the IAM session\": the session must hold the identity the token named"
    );

    // What such a login is NOT admitted for. The rows span the two halves of the decision,
    // every claim the token carries, and the transport it was offered over.
    for (case, transport, user, token) in [
        (
            "an account nothing enabled for it",
            Transport::Tls,
            "plainuser",
            auth_token(&address, port, "plainuser", REGION, now, 900),
        ),
        (
            "a token generated for another user",
            Transport::Tls,
            IAM_USER,
            auth_token(&address, port, "someone", REGION, now, 900),
        ),
        (
            "a token generated for another port",
            Transport::Tls,
            IAM_USER,
            auth_token(&address, port + 1, IAM_USER, REGION, now, 900),
        ),
        (
            "a token signed in another Region",
            Transport::Tls,
            IAM_USER,
            auth_token(&address, port, IAM_USER, "eu-west-1", now, 900),
        ),
        (
            "a token whose window has passed",
            Transport::Tls,
            IAM_USER,
            auth_token(
                &address,
                port,
                IAM_USER,
                REGION,
                now - Duration::from_secs(3_600),
                900,
            ),
        ),
        (
            "a password that is no token at all",
            Transport::Tls,
            IAM_USER,
            "not-a-token".to_string(),
        ),
        (
            // The server asks for the token only inside a TLS session, so a login in the
            // clear never presents one and is refused for the password it has none of.
            "a token offered on a connection in the clear",
            Transport::Plaintext,
            IAM_USER,
            auth_token(&address, port, IAM_USER, REGION, now, 900),
        ),
    ] {
        let refusal = mysql_login(&address, port, transport, user, &token, Some(DATABASE))
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: expected the login to be refused"));
        assert_eq!(
            refusal,
            Refusal {
                code: "1045".to_string(),
                message: format!(
                    "Access denied for user '{user}'@'{address}' (using password: YES)"
                ),
            },
            "case {case:?}: expected MySQL's own authentication refusal, got {refusal:?}"
        );
    }
}

#[tokio::test]
async fn a_token_admits_no_mysql_login_on_an_instance_that_does_not_serve_iam_authentication() {
    let router = emulator();
    // The same engine-side account, on an instance whose control plane leaves IAM database
    // authentication OFF: both halves have to hold, so the token admits nobody.
    let (address, port) =
        available_instance(&router, "emu-mysql-iam-off", "mysql", &params![]).await;
    let mut session = master_session(&address, port).await;
    run(
        "the IAM account",
        &mut session,
        &format!("CREATE USER '{IAM_USER}'@'%' IDENTIFIED WITH AWSAuthenticationPlugin AS 'RDS'"),
    )
    .await;
    let token = auth_token(&address, port, IAM_USER, REGION, SystemTime::now(), 900);
    let refusal = mysql_login(
        &address,
        port,
        Transport::Tls,
        IAM_USER,
        &token,
        Some(DATABASE),
    )
    .await
    .err()
    .expect("an instance that does not serve IAM authentication must refuse the login");
    assert_eq!(
        refusal.code, "1045",
        "expected MySQL's authentication refusal, got {refusal:?}"
    );
}
