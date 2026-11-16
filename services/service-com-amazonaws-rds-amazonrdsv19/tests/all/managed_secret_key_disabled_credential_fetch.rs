// SPDX-License-Identifier: CC0-1.0

//! What the KMS key of an RDS-managed master user secret gates, and what it does NOT.
//!
//! THE RULE under test, asserted over a table of inputs rather than one recorded call: a
//! resource created with `ManageMasterUserPassword` on reports a `MasterUserSecret`
//! (`SecretArn`, `SecretStatus`, `KmsKeyId`) whose status leaves `creating` for `active` in
//! bounded time, after which the CREDENTIAL FETCH on that ARN serves the full credential
//! document — and that fetch goes through the KMS key the secret is encrypted under, so
//! while that key cannot be used the fetch is refused, whereas a database session is
//! authenticated by the ENGINE against the master credential already installed on it and is
//! untouched: a session already open keeps reading and writing its own database, and the
//! resource keeps reporting the same secret.
//!
//! What follows from that one rule, and is asserted on every row:
//!
//! * the create answers with the secret still `creating`, and a client polling the
//!   REPORTING call sees it reach `active` — the wait a managed-credential client's own
//!   poll loop makes, through the shared poll helper
//!   ([`crate::lifecycle_poll_loops::poll_until`]), so the CONDITION is what ends it;
//! * the `KmsKeyId` reported is the key the request named, whichever identifier it named it
//!   by, as KMS's own ARN for that key;
//! * the credentials the secret then holds are the master user's, and they open a session on
//!   the instance's own published engine endpoint;
//! * with the key DISABLED, `GetSecretValue` on that ARN is refused with Secrets Manager's
//!   decryption refusal, while the session opened before still runs statements against the
//!   instance's own storage and the instance still reports the secret as `active` under that
//!   key — a key being unusable is not a change to the resource;
//! * with the key enabled again, the fetch serves the SAME credentials and they still open a
//!   fresh session, which sees the rows the open session wrote.
//!
//! The rows differ along what the rule generalises over: the engine whose wire the session
//! speaks (RDS for PostgreSQL over TLS, RDS for MySQL and for MariaDB over the MySQL
//! protocol) and how the request identifies the key for the secret (its id, its ARN, an
//! alias of it).
//!
//! Secrets Manager and KMS are MOUNTED beside the RDS service over one engine
//! ([`crate::managed_master_user_password::emulator`]), because the handlers call them: the
//! secret read here is a real secret of this emulator, the key is a real KMS key, and the
//! endpoint the session is opened on is the instance's own listener on this machine.

use aws_sdk_secretsmanager::operation::get_secret_value::GetSecretValueError;
use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_engine_endpoint::{
    endpoint_of, mysql_login, postgres_login, MySqlSession, PostgresSession, Rows, Transport,
    DATABASE, MASTER_USER,
};
use crate::db_instances::{call, expect_ok, optional_text, subtrees, text};
use crate::lifecycle_poll_loops::poll_until;
use crate::managed_master_user_password::{
    emulator, held_credentials, kms, master_user_secret, secrets_manager,
};

/// `DBInstance$DBInstanceStatus` once the instance can serve: a session is opened only on an
/// instance that reports it, exactly as a client waits for it.
const AVAILABLE: &str = "available";

/// The table the open session writes into, on the instance's OWN database.
const TABLE: &str = "emu_open_session";

/// Which native wire a row's session speaks, and so which client opens it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wire {
    /// The PostgreSQL protocol. RDS for PostgreSQL forces TLS by default, so the session is
    /// negotiated in band on the same port.
    Postgres,
    /// The MySQL protocol, which RDS for MySQL and for MariaDB both speak.
    MySql,
}

/// How a row's request identifies the key the secret is encrypted under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyForm {
    /// `MasterUserSecretKmsKeyId` carries the key's id.
    Id,
    /// It carries the key's ARN.
    Arn,
    /// It carries an alias of the key.
    Alias,
}

/// One row: the instance to create, the wire its session speaks, and how its request names
/// the key for its secret.
struct Row {
    case: &'static str,
    engine: &'static str,
    instance_class: &'static str,
    wire: Wire,
    key_form: KeyForm,
}

const ROWS: &[Row] = &[
    Row {
        case: "an RDS for PostgreSQL instance whose secret names the key by its id",
        engine: "postgres",
        instance_class: "db.t4g.micro",
        wire: Wire::Postgres,
        key_form: KeyForm::Id,
    },
    Row {
        case: "an RDS for MySQL instance whose secret names the key by its ARN",
        engine: "mysql",
        instance_class: "db.t3.micro",
        wire: Wire::MySql,
        key_form: KeyForm::Arn,
    },
    Row {
        case: "an RDS for MariaDB instance whose secret names an alias of the key",
        engine: "mariadb",
        instance_class: "db.t3.small",
        wire: Wire::MySql,
        key_form: KeyForm::Alias,
    },
];

/// A session a client has opened on an instance's published engine endpoint, over whichever
/// wire that instance speaks.
enum Session {
    Postgres(PostgresSession),
    MySql(MySqlSession),
}

impl Session {
    /// Run one statement on the open session, which must be served.
    async fn run(&mut self, case: &str, sql: &str) -> Rows {
        let answered = match self {
            Session::Postgres(session) => session.query(sql).await,
            Session::MySql(session) => session.query(sql).await,
        };
        answered.unwrap_or_else(|refusal| {
            panic!("case {case:?}: {sql:?} must run on the open session, got {refusal:?}")
        })
    }

    /// The one value a single-row, single-column statement answered with.
    async fn value_of(&mut self, case: &str, sql: &str) -> String {
        let answered = self.run(case, sql).await;
        answered
            .only()
            .unwrap_or_else(|| panic!("case {case:?}: {sql:?} must answer one value, got {answered:?}"))
            .to_string()
    }
}

/// Open a session as the master user with `password`, which must be admitted.
async fn login(case: &str, row: &Row, address: &str, port: u16, password: &str) -> Session {
    match row.wire {
        Wire::Postgres => Session::Postgres(
            postgres_login(
                address,
                port,
                Transport::Tls,
                MASTER_USER,
                password,
                Some(DATABASE),
            )
            .await
            .unwrap_or_else(|refusal| {
                panic!("case {case:?}: the credentials the secret holds must open a PostgreSQL session, got {refusal:?}")
            }),
        ),
        Wire::MySql => Session::MySql(
            mysql_login(
                address,
                port,
                Transport::Plaintext,
                MASTER_USER,
                password,
                Some(DATABASE),
            )
            .await
            .unwrap_or_else(|refusal| {
                panic!("case {case:?}: the credentials the secret holds must open a MySQL session, got {refusal:?}")
            }),
        ),
    }
}

/// `DescribeDBInstances` for one instance.
async fn describe(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    expect_ok(
        case,
        call(
            router,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", identifier.to_string())],
        )
        .await,
    )
}

/// Poll the REPORTING call until the instance is available AND the secret it reports has
/// reached `active`, answering the report it settled on.
///
/// This is the loop a client that opted into managed master credentials writes: it waits for
/// the CONDITION — a status this service's clock owes — and never for a fixed time.
async fn poll_until_secret_is_active(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> String {
    let params = [("DBInstanceIdentifier", identifier.to_string())];
    let (body, _polls) = poll_until(
        case,
        &format!("{identifier} reporting MasterUserSecret.SecretStatus active"),
        || call(router, "DescribeDBInstances", &params),
        |status, body| {
            status == 200
                && optional_text(body, "DBInstanceStatus").as_deref() == Some(AVAILABLE)
                && reported_secret_status(body).as_deref()
                    == Some(st::MASTER_USER_SECRET_STATUS_ACTIVE)
        },
    )
    .await;
    body
}

/// The `SecretStatus` of the one `MasterUserSecret` a report carries, where it carries one.
fn reported_secret_status(body: &str) -> Option<String> {
    subtrees(body, "MasterUserSecret")
        .first()
        .and_then(|secret| optional_text(secret, "SecretStatus"))
}

/// `CreateDBInstance` for a row, with the management of the master user password on, no
/// `MasterUserPassword` of its own, and the key named in the row's own form.
async fn create_managed_instance(
    router: &MultiServiceRouter,
    row: &Row,
    identifier: &str,
    named_key: &str,
) -> String {
    expect_ok(
        row.case,
        call(
            router,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", identifier.to_string()),
                ("Engine", row.engine.to_string()),
                ("DBInstanceClass", row.instance_class.to_string()),
                ("AllocatedStorage", "20".to_string()),
                ("MasterUsername", MASTER_USER.to_string()),
                ("DBName", DATABASE.to_string()),
                ("ManageMasterUserPassword", "true".to_string()),
                ("MasterUserSecretKmsKeyId", named_key.to_string()),
            ],
        )
        .await,
    )
}

/// The one `<MasterUserSecret>` an envelope must carry.
fn the_secret(case: &str, envelope: &str) -> String {
    master_user_secret(case, envelope).unwrap_or_else(|| {
        panic!("case {case:?}: a resource that manages its master user password must report a MasterUserSecret, got {envelope}")
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disabled_secret_key_blocks_the_credential_fetch_and_not_an_open_session() {
    let router = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let identifier = format!("emu-secret-cmk-{index}");
        // A key of the account's own, through KMS itself, plus an alias naming it: one key,
        // three identifiers of it, and each row names it in its own form.
        let minted = kms(&router)
            .create_key()
            .description("the key a managed master user secret is encrypted under")
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {:?}: KMS must mint a customer managed key: {error}", row.case)
            });
        let metadata = minted
            .key_metadata()
            .unwrap_or_else(|| panic!("case {:?}: a minted key reports its metadata", row.case));
        let key_id = metadata.key_id().to_string();
        let key_arn = metadata
            .arn()
            .unwrap_or_else(|| panic!("case {:?}: a minted key reports its ARN", row.case))
            .to_string();
        let alias = format!("alias/emu-secret-cmk-{index}");
        kms(&router)
            .create_alias()
            .alias_name(&alias)
            .target_key_id(&key_id)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {:?}: KMS must name an alias for the key: {error}", row.case)
            });
        let named_key = match row.key_form {
            KeyForm::Id => key_id.clone(),
            KeyForm::Arn => key_arn.clone(),
            KeyForm::Alias => alias.clone(),
        };

        // (1) The create mints the secret and reports it as still being created, under the
        //     key the request named — as KMS's own ARN for that key, whichever identifier
        //     the request used.
        let created = create_managed_instance(&router, row, &identifier, &named_key).await;
        let (address, port) = endpoint_of(&created);
        let created_secret = the_secret(row.case, &created);
        let secret_arn = text(&created_secret, "SecretArn");
        assert!(
            !secret_arn.is_empty(),
            "case {:?}: expected the create to publish a SecretArn, got {created_secret}",
            row.case
        );
        assert_eq!(
            text(&created_secret, "SecretStatus"),
            st::MASTER_USER_SECRET_STATUS_CREATING,
            "case {:?}: a secret this call just minted is still being created, got \
             {created_secret}",
            row.case
        );
        assert_eq!(
            text(&created_secret, "KmsKeyId"),
            key_arn,
            "case {:?}: expected the secret to report the key the request named as {named_key:?}, \
             got {created_secret}",
            row.case
        );

        // (2) The status the clock owes settles, and the describe reports the SAME secret:
        //     the wait a managed-credential client makes ends on the condition.
        let settled = poll_until_secret_is_active(&router, row.case, &identifier).await;
        let settled_secret = the_secret(row.case, &settled);
        assert_eq!(
            text(&settled_secret, "SecretArn"),
            secret_arn,
            "case {:?}: expected the describe to report the secret the create minted, got \
             {settled_secret}",
            row.case
        );
        assert_eq!(
            text(&settled_secret, "KmsKeyId"),
            key_arn,
            "case {:?}: expected the describe to report the same encryption key, got \
             {settled_secret}",
            row.case
        );

        // (3) The credential fetch serves the generated document, and those credentials open
        //     a session on the instance's own published endpoint.
        let held = held_credentials(row.case, &router, &secret_arn).await;
        assert_eq!(
            held.username.as_deref(),
            Some(MASTER_USER),
            "case {:?}: expected the secret to hold the master user the request named, got {:?}",
            row.case,
            held.username
        );
        assert_eq!(
            st::check_master_user_password(row.engine, &held.password),
            Ok(()),
            "case {:?}: the generated password must satisfy MasterUserPassword for engine {}",
            row.case,
            row.engine
        );
        let mut session = login(row.case, row, &address, port, &held.password).await;
        session
            .run(row.case, &format!("CREATE TABLE {TABLE} (id INTEGER)"))
            .await;
        session
            .run(row.case, &format!("INSERT INTO {TABLE} VALUES (1)"))
            .await;
        assert_eq!(
            session
                .value_of(row.case, &format!("SELECT count(*) FROM {TABLE}"))
                .await,
            "1",
            "case {:?}: the open session must read the row it just wrote",
            row.case
        );

        // (4) With the key disabled, the FETCH is refused — and nothing else is.
        kms(&router)
            .disable_key()
            .key_id(&key_id)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {:?}: KMS must disable the key: {error}", row.case)
            });
        let refused = secrets_manager(&router)
            .get_secret_value()
            .secret_id(&secret_arn)
            .send()
            .await
            .err()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the credential fetch to be refused while the key it is \
                     encrypted under is disabled",
                    row.case
                )
            });
        let service_error = refused.into_service_error();
        assert!(
            matches!(service_error, GetSecretValueError::DecryptionFailure(_)),
            "case {:?}: expected the decryption refusal while the key is disabled, got \
             {service_error:?}",
            row.case
        );

        // The session opened before keeps reading AND writing: it was authenticated by the
        // engine, which asks KMS nothing.
        session
            .run(row.case, &format!("INSERT INTO {TABLE} VALUES (2)"))
            .await;
        assert_eq!(
            session
                .value_of(row.case, &format!("SELECT count(*) FROM {TABLE}"))
                .await,
            "2",
            "case {:?}: a session already open must keep serving while the secret's key is \
             disabled",
            row.case
        );
        // And the resource is unchanged: a key that cannot be used is not a change to the
        // instance or to the secret it reports.
        let during = describe(&router, row.case, &identifier).await;
        let during_secret = the_secret(row.case, &during);
        assert_eq!(
            text(&during_secret, "SecretStatus"),
            st::MASTER_USER_SECRET_STATUS_ACTIVE,
            "case {:?}: expected the instance to keep reporting its secret as active, got \
             {during_secret}",
            row.case
        );
        assert_eq!(
            text(&during_secret, "KmsKeyId"),
            key_arn,
            "case {:?}: expected the instance to keep reporting the same key, got {during_secret}",
            row.case
        );

        // (5) Enabled again, the fetch serves the SAME credentials, and they still open a
        //     fresh session — which sees what the open one wrote.
        kms(&router)
            .enable_key()
            .key_id(&key_id)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {:?}: KMS must enable the key again: {error}", row.case)
            });
        let reread = held_credentials(row.case, &router, &secret_arn).await;
        assert_eq!(
            reread.password, held.password,
            "case {:?}: the same credentials must come back once the key can be used again",
            row.case
        );
        let mut reopened = login(row.case, row, &address, port, &reread.password).await;
        assert_eq!(
            reopened
                .value_of(row.case, &format!("SELECT count(*) FROM {TABLE}"))
                .await,
            "2",
            "case {:?}: a session reopened with the fetched credentials must see the rows the \
             first one wrote",
            row.case
        );
    }
}
