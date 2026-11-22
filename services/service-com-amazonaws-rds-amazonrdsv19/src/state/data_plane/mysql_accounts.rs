// SPDX-License-Identifier: CC0-1.0

//! The ACCOUNTS of a MySQL-compatible DB instance: the database users a session creates, the
//! authentication plugin each of them authenticates with, and the privileges granted to them.
//!
//! A MySQL account is `user@host-pattern` and belongs to the SERVER, not to one database, so
//! the catalog is kept in the `mysql` system schema every MySQL server has — the table
//! `mysql.user`, with the columns a MySQL server keeps an account's identity in (`User`,
//! `Host`, `plugin`, `authentication_string`), and the privileges in
//! `information_schema.USER_PRIVILEGES`'s own columns (`GRANTEE`, `PRIVILEGE_TYPE`). They are
//! real tables in the instance's own storage, so what a `CREATE USER` wrote is what a later
//! session's `SHOW GRANTS` reports and what a LOGIN is admitted against.
//!
//! What it is for: IAM database authentication on a MySQL engine is an authentication PLUGIN
//! on the account. "To use IAM database authentication ... create a database account that
//! authenticates with the `AWSAuthenticationPlugin`" — `CREATE USER <account> IDENTIFIED WITH
//! AWSAuthenticationPlugin AS 'RDS'` (Amazon RDS User Guide, "Creating and using an IAM
//! policy for IAM database access") — and a login as such an account presents an
//! authentication token instead of a password. So the plugin recorded here is the engine-side
//! half of `EnableIAMDatabaseAuthentication`, exactly as membership of `rds_iam` is on the
//! PostgreSQL wire ([`super::pg_grants::IAM_AUTHENTICATION_ROLE`]), and
//! [`authenticates_with_a_token`] is what the wire's login reads.
//!
//! Every statement here is the SERVER's rather than the engine's: MySQL's account grammar is
//! not SQL the embedded engine parses, and the catalog it writes is this server's own.
//!
//! What is NOT here: the PASSWORD half of an account. An `IDENTIFIED BY '<password>'` names a
//! password this server does not record and nothing admits a login against, because the only
//! credentials this endpoint admits are the master user's own password
//! ([`super::wire_session::admit`]) and an IAM authentication token — so an account created
//! with a password of its own exists, holds its grants and is reported, and a login as it is
//! refused. Serving such a login is a separate rule about how an engine stores and checks a
//! password, which this module does not claim to implement.

use ::sqlparser::tokenizer::Token;

use super::sql_engine::{Cell, ColumnInfo, EngineError, EngineErrorKind, InstanceDatabase, ResultSet};
use super::wire_session::{SessionFacts, StatementAnswer, MYSQL_MASTER_HOST_PATTERN};

/// The system schema a MySQL server keeps its accounts in, which is where this catalog lives:
/// the accounts belong to the server, so they are not in any one of the instance's databases.
const ACCOUNT_DATABASE: &str = "mysql";

/// The catalog tables, with the columns MySQL publishes for each.
///
/// Source: the MySQL Reference Manual's "Grant Tables" (`mysql.user`: `User`, `Host`,
/// `plugin`, `authentication_string`) and "The INFORMATION_SCHEMA USER_PRIVILEGES Table"
/// (`GRANTEE`, `PRIVILEGE_TYPE`, `IS_GRANTABLE`), reduced to the columns this server has
/// state behind. `scope` carries the `ON` clause of the grant that wrote the row, which is
/// what `SHOW GRANTS` renders the statement back from.
///
/// The server CREATES them where it starts, with the rest of the schema it comes up with
/// ([`super::mysql_system_tables`]); they are created here too, so a server whose schema was
/// restored from storage that predates them still records an account.
pub(super) const CATALOG_SCHEMA: [&str; 2] = [
    "CREATE TABLE IF NOT EXISTS user (\
       User text NOT NULL, Host text NOT NULL, plugin text, authentication_string text, \
       PRIMARY KEY (User, Host))",
    "CREATE TABLE IF NOT EXISTS user_privileges (\
       GRANTEE text NOT NULL, scope text NOT NULL, PRIVILEGE_TYPE text NOT NULL, \
       IS_GRANTABLE text, PRIMARY KEY (GRANTEE, scope, PRIVILEGE_TYPE))",
];

/// The authentication plugin an account authenticates with an IAM authentication token
/// through, as `CREATE USER ... IDENTIFIED WITH` names it.
pub const IAM_AUTHENTICATION_PLUGIN: &str = "AWSAuthenticationPlugin";

/// The plugin a MySQL account authenticates with a password through, which is what an account
/// created with no plugin named gets.
const NATIVE_AUTHENTICATION_PLUGIN: &str = "mysql_native_password";

/// How many accounts this server holds, and how many privilege rows one account may hold: a
/// client chooses both, so the catalog is bounded rather than growing with whatever a session
/// writes. Both are far above what a database's user model needs.
const MAX_ACCOUNTS: i64 = 256;
const MAX_PRIVILEGE_ROWS: i64 = 4_096;

/// One account named by a statement: `user@host-pattern`, as MySQL writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// The user half.
    pub user: String,
    /// The host-pattern half, which is `%` for an account written without one — the pattern
    /// MySQL gives an account named by its user alone.
    pub host: String,
}

impl Account {
    /// The account as `GRANTEE` holds it, which is the form MySQL's own catalog keys a
    /// privilege row by: `'user'@'host'`.
    #[must_use]
    fn grantee(&self) -> String {
        format!("'{}'@'{}'", self.user, self.host)
    }
}

/// Serve the ACCOUNT statements of a MySQL server: the statements that write this catalog,
/// and `FLUSH PRIVILEGES`, which re-reads it.
///
/// `None` means the statement is not one of them, and is the engine's to run.
pub fn account_statement(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    words: &[String],
    tokens: &[Token],
) -> Result<Option<StatementAnswer>, EngineError> {
    let first = words.first().map(String::as_str).unwrap_or_default();
    let second = words.get(1).map(String::as_str).unwrap_or_default();
    match (first, second) {
        ("CREATE", "USER") => create_user(database, facts, tokens).map(Some),
        ("ALTER", "USER") => alter_user(database, facts, tokens).map(Some),
        ("DROP", "USER") => drop_user(database, facts, tokens).map(Some),
        ("GRANT", _) | ("REVOKE", _) => match parsed_privileges(&session_account(facts), tokens) {
            Some(statement) => {
                privilege_statement(database, facts, first, &statement).map(Some)
            }
            // A privilege statement this server does not READ is not one it serves: the
            // forms it records are the ones above, and a grant of anything else — a ROLE
            // granted to an account, which this server keeps no roles for — is left to the
            // engine, which refuses it rather than reporting a success that stored nothing.
            None => Ok(None),
        },
        // Re-reading the grant tables is what `FLUSH PRIVILEGES` does, and this server reads
        // them per statement: there is nothing cached for it to drop, so it is the success
        // MySQL answers for it. The RDS master user holds the `RELOAD` privilege it needs.
        // Only that form: every other `FLUSH` does something of its own — takes a lock,
        // rotates a log, clears a cache — which this server does not do, so it is left to
        // the engine's refusal rather than reported as performed.
        ("FLUSH", "PRIVILEGES") => Ok(Some(StatementAnswer::new(
            "FLUSH",
            ResultSet::changed(0, 0),
        ))),
        _ => Ok(None),
    }
}

/// `CREATE USER [IF NOT EXISTS] <account> [IDENTIFIED WITH <plugin> [AS '<auth>']] [, …]`.
///
/// The account rows go in the catalog, so a later session reads the accounts this one created.
/// An account the server already holds is MySQL's own duplicate-user refusal unless the
/// statement said `IF NOT EXISTS`, in which case it is a no-op.
fn create_user(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    tokens: &[Token],
) -> Result<StatementAnswer, EngineError> {
    ensure_catalogs(database, facts)?;
    let statement = parsed(&session_account(facts), tokens);
    if statement.accounts.is_empty() {
        return Err(syntax_error(tokens));
    }
    let plugin = statement
        .plugin
        .clone()
        .unwrap_or_else(|| NATIVE_AUTHENTICATION_PLUGIN.to_string());
    for account in &statement.accounts {
        if held(database, facts, account)? {
            if statement.if_not_exists {
                continue;
            }
            return Err(EngineError::new(
                EngineErrorKind::DuplicateObject,
                format!(
                    "Operation CREATE USER failed for '{}'@'{}'",
                    account.user, account.host
                ),
            )
            .with_mysql_error(1396, "HY000"));
        }
        admit_another_account(database, facts)?;
        database.execute_in(
            facts.connection_id,
            ACCOUNT_DATABASE,
            "INSERT INTO user (User, Host, plugin, authentication_string) VALUES (?, ?, ?, ?)",
            &[
                Some(account.user.clone()),
                Some(account.host.clone()),
                Some(plugin.clone()),
                statement.authentication.clone(),
            ],
        )?;
    }
    Ok(StatementAnswer::new(
        "CREATE USER",
        ResultSet::changed(0, 0),
    ))
}

/// `ALTER USER [IF EXISTS] <account> IDENTIFIED WITH <plugin> [AS '<auth>']`: the account
/// keeps its identity and authenticates with the plugin the statement names from now on.
fn alter_user(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    tokens: &[Token],
) -> Result<StatementAnswer, EngineError> {
    ensure_catalogs(database, facts)?;
    let statement = parsed(&session_account(facts), tokens);
    if statement.accounts.is_empty() {
        return Err(syntax_error(tokens));
    }
    for account in &statement.accounts {
        if !held(database, facts, account)? {
            if statement.if_exists {
                continue;
            }
            return Err(unknown_account("ALTER USER", account));
        }
        if let Some(plugin) = &statement.plugin {
            database.execute_in(
                facts.connection_id,
                ACCOUNT_DATABASE,
                "UPDATE user SET plugin = ?, authentication_string = ? \
                 WHERE User = ? AND Host = ?",
                &[
                    Some(plugin.clone()),
                    statement.authentication.clone(),
                    Some(account.user.clone()),
                    Some(account.host.clone()),
                ],
            )?;
        }
    }
    Ok(StatementAnswer::new("ALTER USER", ResultSet::changed(0, 0)))
}

/// `DROP USER [IF EXISTS] <account> [, …]`: the account and every privilege granted to it go.
fn drop_user(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    tokens: &[Token],
) -> Result<StatementAnswer, EngineError> {
    ensure_catalogs(database, facts)?;
    let statement = parsed(&session_account(facts), tokens);
    if statement.accounts.is_empty() {
        return Err(syntax_error(tokens));
    }
    for account in &statement.accounts {
        if !held(database, facts, account)? {
            if statement.if_exists {
                continue;
            }
            return Err(unknown_account("DROP USER", account));
        }
        database.execute_in(
            facts.connection_id,
            ACCOUNT_DATABASE,
            "DELETE FROM user WHERE User = ? AND Host = ?",
            &[Some(account.user.clone()), Some(account.host.clone())],
        )?;
        database.execute_in(
            facts.connection_id,
            ACCOUNT_DATABASE,
            "DELETE FROM user_privileges WHERE GRANTEE = ?",
            &[Some(account.grantee())],
        )?;
    }
    Ok(StatementAnswer::new("DROP USER", ResultSet::changed(0, 0)))
}

/// `GRANT <privileges> ON <scope> TO <account> [, …]` and the `REVOKE … FROM` that takes them
/// back, as `statement` was read from it.
///
/// MySQL 8 creates no account for a grant: a statement naming one the server does not hold is
/// refused in its own words, so a privilege row always belongs to an account that exists.
fn privilege_statement(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    verb: &str,
    statement: &PrivilegeStatement,
) -> Result<StatementAnswer, EngineError> {
    ensure_catalogs(database, facts)?;
    for account in &statement.accounts {
        if !held(database, facts, account)? {
            // What MySQL answers a grant naming an account it does not hold: it will not
            // create one.
            return Err(EngineError::new(
                EngineErrorKind::InsufficientPrivilege,
                "You are not allowed to create a user with GRANT".to_string(),
            )
            .with_mysql_error(1410, "42000"));
        }
    }
    for account in &statement.accounts {
        for privilege in &statement.privileges {
            if verb == "GRANT" {
                admit_another_privilege(database, facts)?;
                database.execute_in(
                    facts.connection_id,
                    ACCOUNT_DATABASE,
                    "INSERT OR REPLACE INTO user_privileges \
                       (GRANTEE, scope, PRIVILEGE_TYPE, IS_GRANTABLE) VALUES (?, ?, ?, 'NO')",
                    &[
                        Some(account.grantee()),
                        Some(statement.scope.clone()),
                        Some(privilege.clone()),
                    ],
                )?;
            } else {
                database.execute_in(
                    facts.connection_id,
                    ACCOUNT_DATABASE,
                    "DELETE FROM user_privileges \
                     WHERE GRANTEE = ? AND scope = ? AND PRIVILEGE_TYPE = ?",
                    &[
                        Some(account.grantee()),
                        Some(statement.scope.clone()),
                        Some(privilege.clone()),
                    ],
                )?;
            }
        }
    }
    Ok(StatementAnswer::new(verb, ResultSet::changed(0, 0)))
}

/// The `GRANT` statements `SHOW GRANTS FOR <account>` reports for an account this catalog
/// holds, or `None` for an account it holds none of — which the wire answers with MySQL's own
/// "no such grant" refusal.
///
/// An account that exists always has one: "USAGE" is MySQL's "no privileges" grant, which
/// every account holds on `*.*`, and the privileges granted to it follow, one statement per
/// scope they were granted on.
///
/// A READ, and nothing else: the catalog is not created here, so asking about the accounts of
/// an instance no statement has created one on leaves nothing behind — a server whose catalog
/// does not exist holds no account, which is the `None` this answers.
pub fn grants_of(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    account: &Account,
) -> Result<Option<ResultSet>, EngineError> {
    if !holds(database, facts, account) {
        return Ok(None);
    }
    let granted = database.execute_in(
        facts.connection_id,
        ACCOUNT_DATABASE,
        "SELECT scope, PRIVILEGE_TYPE FROM user_privileges WHERE GRANTEE = ? \
         ORDER BY scope, PRIVILEGE_TYPE",
        &[Some(account.grantee())],
    )?;
    let mut scopes: Vec<(String, Vec<String>)> = Vec::new();
    for row in &granted.rows {
        let scope = text(row.first());
        let privilege = text(row.get(1));
        match scopes.iter_mut().find(|(held, _)| *held == scope) {
            Some((_, privileges)) => privileges.push(privilege),
            None => scopes.push((scope, vec![privilege])),
        }
    }
    let mut statements = vec![format!(
        "GRANT USAGE ON *.* TO `{}`@`{}`",
        account.user, account.host
    )];
    for (scope, privileges) in scopes {
        statements.push(format!(
            "GRANT {} ON {scope} TO `{}`@`{}`",
            privileges.join(", "),
            account.user,
            account.host
        ));
    }
    Ok(Some(ResultSet::of_rows(
        vec![ColumnInfo {
            name: format!("Grants for {}@{}", account.user, account.host),
            declared_type: Some("text".to_string()),
        }],
        statements
            .into_iter()
            .map(|statement| vec![Cell::Text(statement)])
            .collect(),
    )))
}

/// Whether the database user `user` is one enabled for IAM database authentication: an
/// account of this server whose authentication plugin is [`IAM_AUTHENTICATION_PLUGIN`] — the
/// engine-side half of `EnableIAMDatabaseAuthentication` on a MySQL engine.
///
/// The account is resolved by its USER and its plugin, and not by its host pattern: a MySQL
/// host pattern is matched against the address the CLIENT connects from, and no session of
/// this endpoint has one to match — a client of this emulator reaches it on the loopback
/// address the endpoint is itself bound to, or through the in-process pipe
/// (`::service_common::dialable_data_planes`), which has no address at all. Resolving a pattern
/// against the SERVER's own address would admit or refuse logins on a value that says nothing
/// about the client, so the plugin alone decides, and every account of that user name is
/// enabled for the token.
///
/// A READ, and nothing else: the catalog is not created here, so a login decided before any
/// session has created an account leaves nothing behind. It is taken without a session,
/// because a login HAS no session yet, so what it reads is the committed catalog. A server
/// whose catalog does not exist yet holds no account, so no user of it authenticates with a
/// token — which is what the unreadable case answers, since a login is admitted on what the
/// catalog SAYS and never on a question that could not be asked.
#[must_use]
pub fn authenticates_with_a_token(database: &InstanceDatabase, user: &str) -> bool {
    database
        .execute(
            ACCOUNT_DATABASE,
            "SELECT 1 FROM user WHERE User = ? AND plugin = ?",
            &[
                Some(user.to_string()),
                Some(IAM_AUTHENTICATION_PLUGIN.to_string()),
            ],
        )
        .is_ok_and(|found| !found.rows.is_empty())
}

/// Create the catalog tables if this instance has not held an account yet, in the system
/// schema they belong to — which is created with them, as a MySQL server comes with it.
fn ensure_catalogs(
    database: &InstanceDatabase,
    facts: &SessionFacts,
) -> Result<(), EngineError> {
    database.create_database(ACCOUNT_DATABASE)?;
    for schema in CATALOG_SCHEMA {
        database.execute_in(facts.connection_id, ACCOUNT_DATABASE, schema, &[])?;
    }
    Ok(())
}

/// Whether the catalog holds `account`.
fn held(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    account: &Account,
) -> Result<bool, EngineError> {
    let found = database.execute_in(
        facts.connection_id,
        ACCOUNT_DATABASE,
        "SELECT 1 FROM user WHERE User = ? AND Host = ?",
        &[Some(account.user.clone()), Some(account.host.clone())],
    )?;
    Ok(!found.rows.is_empty())
}

/// The same question, asked WITHOUT creating the catalog: a server whose catalog does not
/// exist holds no account, which is what the unreadable case answers.
fn holds(database: &InstanceDatabase, facts: &SessionFacts, account: &Account) -> bool {
    database
        .execute_in(
            facts.connection_id,
            ACCOUNT_DATABASE,
            "SELECT 1 FROM user WHERE User = ? AND Host = ?",
            &[Some(account.user.clone()), Some(account.host.clone())],
        )
        .is_ok_and(|found| !found.rows.is_empty())
}

/// Whether one more account may be created; past the cap a further `CREATE USER` is refused
/// rather than growing the catalog without bound.
fn admit_another_account(
    database: &InstanceDatabase,
    facts: &SessionFacts,
) -> Result<(), EngineError> {
    admit_another_row(database, facts, "user", MAX_ACCOUNTS, "accounts")
}

/// The same bound on the privilege rows one server holds.
fn admit_another_privilege(
    database: &InstanceDatabase,
    facts: &SessionFacts,
) -> Result<(), EngineError> {
    admit_another_row(
        database,
        facts,
        "user_privileges",
        MAX_PRIVILEGE_ROWS,
        "granted privileges",
    )
}

/// Whether one more row may go in `table`, which holds at most `cap` of them.
fn admit_another_row(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    table: &str,
    cap: i64,
    held_what: &str,
) -> Result<(), EngineError> {
    let counted = database.execute_in(
        facts.connection_id,
        ACCOUNT_DATABASE,
        &format!("SELECT COUNT(*) FROM {table}"),
        &[],
    )?;
    let held = counted
        .rows
        .first()
        .and_then(|row| row.first())
        .and_then(|cell| match cell {
            Cell::Integer(count) => Some(*count),
            _ => None,
        })
        .unwrap_or_default();
    if held >= cap {
        return Err(EngineError::new(
            EngineErrorKind::LimitExceeded,
            format!("this emulator's MySQL endpoint holds at most {cap} {held_what}"),
        ));
    }
    Ok(())
}

/// What a statement this server could not read says, in MySQL's own parse-error terms.
fn syntax_error(tokens: &[Token]) -> EngineError {
    let written: String = tokens
        .iter()
        .map(|token| token.to_string())
        .collect::<Vec<String>>()
        .join(" ");
    EngineError::new(
        EngineErrorKind::Syntax,
        format!(
            "You have an error in your SQL syntax; check the manual that corresponds to your \
             MySQL server version for the right syntax to use near '{written}' at line 1"
        ),
    )
}

/// What MySQL answers for a statement naming an account the server does not hold.
fn unknown_account(operation: &str, account: &Account) -> EngineError {
    EngineError::new(
        EngineErrorKind::UndefinedObject,
        format!(
            "Operation {operation} failed for '{}'@'{}'",
            account.user, account.host
        ),
    )
    .with_mysql_error(1396, "HY000")
}

/// One cell as the text it carries.
fn text(cell: Option<&Cell>) -> String {
    match cell {
        Some(Cell::Text(value)) => value.clone(),
        Some(Cell::Integer(value)) => value.to_string(),
        _ => String::new(),
    }
}

/// What a `CREATE`/`ALTER`/`DROP USER` statement names.
#[derive(Debug, Default)]
struct UserStatement {
    /// The accounts it names, in the order it named them.
    accounts: Vec<Account>,
    /// The plugin an `IDENTIFIED WITH` names, if it names one.
    plugin: Option<String>,
    /// What an `AS '<auth>'` carries, which is the `authentication_string` of the account.
    authentication: Option<String>,
    if_not_exists: bool,
    if_exists: bool,
}

/// Read a `CREATE`/`ALTER`/`DROP USER` statement: the accounts it names and the
/// authentication it names for them.
///
/// An account is `'<user>'@'<host>'`, `<user>@<host>` or a bare `<user>` (whose host pattern
/// is `%`, as MySQL gives it), written quoted or not; the plugin is the word after
/// `IDENTIFIED WITH` and the authentication string is what follows `AS`.
fn parsed(session: &Account, tokens: &[Token]) -> UserStatement {
    let mut statement = UserStatement::default();
    let mut at_host = false;
    let mut after_with = false;
    let mut after_as = false;
    let mut in_accounts = true;
    for (position, token) in tokens.iter().enumerate() {
        let keyword = match token {
            Token::Word(word) if word.quote_style.is_none() => {
                Some(word.value.to_ascii_uppercase())
            }
            _ => None,
        };
        match keyword.as_deref() {
            // The leading `CREATE USER` / `DROP USER` / `ALTER USER`, and the modifiers that
            // follow it, name no account.
            Some("CREATE" | "ALTER" | "DROP" | "USER" | "IF") if position < 4 => continue,
            Some("NOT") if position < 4 => {
                statement.if_not_exists = true;
                continue;
            }
            Some("EXISTS") if position < 5 => {
                if !statement.if_not_exists {
                    statement.if_exists = true;
                }
                continue;
            }
            Some("IDENTIFIED") => {
                in_accounts = false;
                continue;
            }
            Some("WITH") => {
                after_with = true;
                continue;
            }
            Some("AS") => {
                after_as = true;
                continue;
            }
            _ => {}
        }
        if let Some(host) = host_word(token) {
            if in_accounts {
                if let Some(account) = statement.accounts.last_mut() {
                    account.host = host;
                }
            }
            at_host = false;
            continue;
        }
        if let Token::AtSign = token {
            at_host = true;
            continue;
        }
        if let Token::Comma = token {
            at_host = false;
            continue;
        }
        let Some(value) = named(token) else {
            continue;
        };
        if after_with {
            statement.plugin = Some(value);
            after_with = false;
            continue;
        }
        if after_as {
            statement.authentication = Some(value);
            after_as = false;
            continue;
        }
        if !in_accounts {
            continue;
        }
        if at_host {
            if let Some(account) = statement.accounts.last_mut() {
                account.host = value;
            }
            at_host = false;
            continue;
        }
        statement.accounts.push(account_from(session, token, &value));
    }
    statement
}

/// What a `GRANT`/`REVOKE` statement names.
#[derive(Debug)]
struct PrivilegeStatement {
    /// The privileges it names, upper-cased as MySQL reports them.
    privileges: Vec<String>,
    /// The `ON` clause, as the statement wrote it (`*.*`, `appdb.*`, `appdb.orders`).
    scope: String,
    /// The accounts it grants to, or revokes from.
    accounts: Vec<Account>,
}

/// Read a `GRANT <privileges> ON <scope> TO <accounts>` or its `REVOKE … FROM` form.
///
/// `None` for a statement with no account list — `GRANT <role> TO <role>`, which grants a
/// ROLE rather than privileges, and anything else this server does not read as a privilege
/// grant: it is answered as the success MySQL answers rather than refused, and writes nothing.
fn parsed_privileges(session: &Account, tokens: &[Token]) -> Option<PrivilegeStatement> {
    let mut privileges: Vec<String> = Vec::new();
    let mut scope: Vec<String> = Vec::new();
    let mut accounts: Vec<Account> = Vec::new();
    let mut section = Section::Privileges;
    let mut at_host = false;
    let mut privilege: Vec<String> = Vec::new();
    for token in tokens {
        let keyword = match token {
            Token::Word(word) if word.quote_style.is_none() => {
                Some(word.value.to_ascii_uppercase())
            }
            _ => None,
        };
        match keyword.as_deref() {
            Some("GRANT" | "REVOKE") if section == Section::Privileges => continue,
            Some("ON") => {
                push_privilege(&mut privileges, &mut privilege);
                section = Section::Scope;
                continue;
            }
            Some("TO" | "FROM") => {
                section = Section::Accounts;
                continue;
            }
            // The trailing options of a grant, which name no account.
            Some("WITH") if section == Section::Accounts => break,
            _ => {}
        }
        match section {
            Section::Privileges => match token {
                Token::Comma => push_privilege(&mut privileges, &mut privilege),
                Token::Word(word) => privilege.push(word.value.to_ascii_uppercase()),
                Token::Mul => privilege.push("*".to_string()),
                _ => {}
            },
            Section::Scope => match token {
                Token::Word(word) => scope.push(word.value.clone()),
                Token::Mul => scope.push("*".to_string()),
                Token::Period => scope.push(".".to_string()),
                _ => {}
            },
            Section::Accounts => {
                if let Some(host) = host_word(token) {
                    if let Some(account) = accounts.last_mut() {
                        account.host = host;
                    }
                    at_host = false;
                    continue;
                }
                if let Token::AtSign = token {
                    at_host = true;
                    continue;
                }
                if let Token::Comma = token {
                    at_host = false;
                    continue;
                }
                let Some(value) = named(token) else { continue };
                if at_host {
                    if let Some(account) = accounts.last_mut() {
                        account.host = value;
                    }
                    at_host = false;
                } else {
                    accounts.push(account_from(session, token, &value));
                }
            }
        }
    }
    if accounts.is_empty() || privileges.is_empty() {
        return None;
    }
    Some(PrivilegeStatement {
        privileges,
        scope: match scope.is_empty() {
            true => "*.*".to_string(),
            false => scope.join(""),
        },
        accounts,
    })
}

/// Which part of a privilege statement a token belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Privileges,
    Scope,
    Accounts,
}

/// One privilege, which may be several words (`SHOW DATABASES`, `CREATE TEMPORARY TABLES`).
fn push_privilege(privileges: &mut Vec<String>, held: &mut Vec<String>) {
    if !held.is_empty() {
        privileges.push(held.join(" "));
        held.clear();
    }
}

/// The NAME a token carries, however it was written: quoted or not.
fn named(token: &Token) -> Option<String> {
    match token {
        Token::SingleQuotedString(value) | Token::DoubleQuotedString(value) => {
            Some(value.clone())
        }
        Token::Word(word) => Some(word.value.clone()),
        _ => None,
    }
}

/// The HOST half an unquoted account carries in ONE token.
///
/// MySQL's own grammar makes `@` part of an identifier (`@variable`, `user@host`), so an
/// account written without quotes arrives as a SINGLE word carrying both halves
/// (`appuser@localhost`) — and, where the tokenizer read the `@` as the start of one, as a
/// word of its own beginning with it — while a quoted one (`'appuser'@'localhost'`) has the
/// `@` as a token between two strings. Every form names the same account, so every form is
/// read.
fn host_word(token: &Token) -> Option<String> {
    match token {
        Token::Word(word) if word.quote_style.is_none() && word.value.starts_with('@') => {
            Some(word.value.trim_start_matches('@').to_string())
        }
        // The tokenizer reads `@name` in a dialect whose identifiers may begin with `@` as
        // one value of that form, which is the shape the host half of an unquoted account
        // arrives in.
        Token::Placeholder(value) if value.starts_with('@') => {
            Some(value.trim_start_matches('@').to_string())
        }
        _ => None,
    }
}

/// The one FUNCTION form MySQL's account grammar takes in place of a written name:
/// `CURRENT_USER`, with or without an empty argument list.
///
/// Source: the MySQL Reference Manual's `user` value — "`SHOW GRANTS FOR CURRENT_USER` (or
/// the equivalent, `SHOW GRANTS FOR CURRENT_USER()`)" — so every statement that names an
/// account (`SHOW GRANTS FOR`, `GRANT`/`REVOKE`, `SET PASSWORD FOR`, `CREATE`/`ALTER`/`DROP
/// USER`) takes it, and the server EVALUATES it to the account the session holds the
/// privileges of rather than reading it as a user of that name. Only an UNQUOTED word is the
/// keyword: `'current_user'` names a user spelled that way.
const SESSION_ACCOUNT_KEYWORD: &str = "current_user";

/// The account the session holds the privileges of, which is what the session's own name for
/// it ([`SESSION_ACCOUNT_KEYWORD`]) stands for and what `CURRENT_USER()` reports: the user the
/// session authenticated as, on the host pattern the privileges are granted to it on.
#[must_use]
pub fn session_account(facts: &SessionFacts) -> Account {
    Account {
        user: facts.user.clone(),
        host: MYSQL_MASTER_HOST_PATTERN.to_string(),
    }
}

/// The account the name `value` stands for, as the token `token` carried it, in a session
/// whose own account is `session`.
///
/// The session's own account is named by the keyword ([`SESSION_ACCOUNT_KEYWORD`]) rather
/// than written out. An UNQUOTED name carrying an `@` names both halves at once
/// ([`host_word`]); every other name is the user's alone, and takes the `%` host pattern
/// MySQL gives an account written without one — so a quoted name keeps every character it was
/// written with, `@` included.
fn account_from(session: &Account, token: &Token, value: &str) -> Account {
    let unquoted = matches!(token, Token::Word(word) if word.quote_style.is_none());
    if unquoted {
        if value.eq_ignore_ascii_case(SESSION_ACCOUNT_KEYWORD) {
            return session.clone();
        }
        if let Some((user, host)) = value.split_once('@') {
            if !user.is_empty() && !host.is_empty() {
                return Account {
                    user: user.to_string(),
                    host: host.to_string(),
                };
            }
        }
    }
    Account {
        user: value.to_string(),
        host: MYSQL_MASTER_HOST_PATTERN.to_string(),
    }
}

/// The ACCOUNT a `SHOW GRANTS [FOR <account>]` names, read from the same forms an account is
/// written in everywhere else.
///
/// A statement that names none is about the session's own account, which is the account the
/// session is connected as.
#[must_use]
pub fn named_account(facts: &SessionFacts, tokens: &[Token]) -> Account {
    let after_for: Vec<&Token> = tokens
        .iter()
        .skip_while(
            |token| !matches!(token, Token::Word(word) if word.value.eq_ignore_ascii_case("FOR")),
        )
        .skip(1)
        .collect();
    let session = session_account(facts);
    let mut account = session.clone();
    let mut at_host = false;
    let mut named_user = false;
    for token in after_for {
        if let Some(host) = host_word(token) {
            account.host = host;
            break;
        }
        if let Token::AtSign = token {
            at_host = true;
            continue;
        }
        let Some(value) = named(token) else { continue };
        if at_host {
            account.host = value;
            break;
        }
        if !named_user {
            let named = account_from(&session, token, &value);
            account.user = named.user;
            account.host = named.host;
            named_user = true;
        }
    }
    account
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::sqlparser::dialect::MySqlDialect;
    use ::sqlparser::tokenizer::Tokenizer;

    /// The tokens of `sql` as the MySQL wire reads them.
    fn tokens(sql: &str) -> Vec<Token> {
        Tokenizer::new(&MySqlDialect {}, sql)
            .tokenize()
            .expect("the statement tokenizes")
    }

    /// The account the session reading every statement below holds the privileges of.
    fn session() -> Account {
        Account {
            user: "dbadmin".to_string(),
            host: MYSQL_MASTER_HOST_PATTERN.to_string(),
        }
    }

    /// RULE: an account is read from every form MySQL writes one in — quoted with a host
    /// pattern, quoted without one, unquoted, and backquoted — and an account written without
    /// a host pattern takes the `%` MySQL gives it. The keyword `CURRENT_USER`, with or
    /// without an empty argument list, is the SESSION's own account rather than a user of
    /// that name; quoted, it is a user of that name.
    #[test]
    fn an_account_is_read_from_every_form_it_is_written_in() {
        let rows: &[(&str, &str, &str, &str)] = &[
            // (case, statement, user, host)
            (
                "quoted, with a host pattern",
                "CREATE USER 'iamuser'@'%' IDENTIFIED WITH AWSAuthenticationPlugin AS 'RDS'",
                "iamuser",
                "%",
            ),
            (
                "quoted, without a host pattern",
                "CREATE USER 'reporting' IDENTIFIED BY 'secret'",
                "reporting",
                "%",
            ),
            (
                "unquoted, with a host pattern",
                "CREATE USER appuser@localhost IDENTIFIED BY 'secret'",
                "appuser",
                "localhost",
            ),
            (
                "backquoted, with a quoted host pattern",
                "CREATE USER `appuser`@'10.0.0.1'",
                "appuser",
                "10.0.0.1",
            ),
            (
                "dropped, unquoted",
                "DROP USER appuser@localhost",
                "appuser",
                "localhost",
            ),
            (
                "the session's own account, by keyword",
                "DROP USER CURRENT_USER",
                "dbadmin",
                "%",
            ),
            (
                "the session's own account, by keyword with an empty argument list",
                "DROP USER current_user()",
                "dbadmin",
                "%",
            ),
            (
                "a user whose name is spelled like the keyword, quoted",
                "DROP USER 'current_user'",
                "current_user",
                "%",
            ),
        ];
        for (case, statement, user, host) in rows {
            let read = parsed(&session(), &tokens(statement));
            let expected = vec![Account {
                user: (*user).to_string(),
                host: (*host).to_string(),
            }];
            assert_eq!(
                read.accounts, expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                read.accounts
            );
        }
    }

    /// One row of the privilege-statement reading below: the statement, and what reading it
    /// must answer.
    struct PrivilegeRow {
        case: &'static str,
        statement: &'static str,
        privileges: &'static [&'static str],
        scope: &'static str,
        user: &'static str,
        host: &'static str,
    }

    /// RULE: a privilege statement is read the same way, whichever form its accounts and its
    /// privileges are written in.
    #[test]
    fn a_privilege_statement_is_read_from_every_form_it_is_written_in() {
        let rows = [
            PrivilegeRow {
                case: "one privilege on every schema",
                statement: "GRANT SELECT ON *.* TO 'iamuser'@'%'",
                privileges: &["SELECT"],
                scope: "*.*",
                user: "iamuser",
                host: "%",
            },
            PrivilegeRow {
                case: "several privileges on one schema",
                statement: "GRANT SELECT, INSERT ON appdb.* TO 'reporting'@'%'",
                privileges: &["SELECT", "INSERT"],
                scope: "appdb.*",
                user: "reporting",
                host: "%",
            },
            PrivilegeRow {
                case: "a privilege written in several words, on an unquoted account",
                statement: "GRANT SHOW DATABASES ON *.* TO appuser@localhost",
                privileges: &["SHOW DATABASES"],
                scope: "*.*",
                user: "appuser",
                host: "localhost",
            },
            PrivilegeRow {
                case: "a privilege on one table, revoked",
                statement: "REVOKE UPDATE ON appdb.widgets FROM 'reporting'@'%'",
                privileges: &["UPDATE"],
                scope: "appdb.widgets",
                user: "reporting",
                host: "%",
            },
            PrivilegeRow {
                case: "a privilege granted to the session's own account, by keyword",
                statement: "GRANT SELECT ON appdb.* TO CURRENT_USER()",
                privileges: &["SELECT"],
                scope: "appdb.*",
                user: "dbadmin",
                host: "%",
            },
        ];
        for row in &rows {
            let case = row.case;
            let read = parsed_privileges(&session(), &tokens(row.statement))
                .unwrap_or_else(|| panic!("case {case:?}: the statement must be read"));
            assert_eq!(
                read.privileges, row.privileges,
                "case {case:?}: expected the privileges {:?}, got {:?}",
                row.privileges, read.privileges
            );
            assert_eq!(
                read.scope, row.scope,
                "case {case:?}: expected the scope {:?}, got {:?}",
                row.scope, read.scope
            );
            assert_eq!(
                read.accounts,
                vec![Account {
                    user: row.user.to_string(),
                    host: row.host.to_string(),
                }],
                "case {case:?}: expected the account {:?}@{:?}, got {:?}",
                row.user,
                row.host,
                read.accounts
            );
        }
    }

    /// RULE: a privilege statement this server does not READ is left to the engine, which is
    /// what refuses it — a form the catalog records nothing for is never answered as a
    /// success.
    #[test]
    fn a_privilege_statement_this_server_does_not_read_is_left_to_the_engine() {
        let rows: &[(&str, &str)] = &[
            // A MySQL 8 ROLE granted to an account: this server keeps no roles, so it reads
            // no privilege list and no `ON` clause.
            ("a role granted to an account", "GRANT 'app_rw' TO 'iamuser'@'%'"),
            ("a role revoked from an account", "REVOKE 'app_rw' FROM 'iamuser'@'%'"),
            // A grant with no grantee at all.
            ("a grant naming no account", "GRANT SELECT ON *.*"),
        ];
        for (case, statement) in rows {
            let read = parsed_privileges(&session(), &tokens(statement));
            assert!(
                read.is_none(),
                "case {case:?}: expected the statement to be left to the engine, got {read:?}"
            );
        }
    }
}

