// SPDX-License-Identifier: CC0-1.0

//! The `aws_s3` extension on a PostgreSQL engine endpoint: the Amazon S3 import and export an
//! S3-integrated RDS for PostgreSQL DB instance serves.
//!
//! The rules these pin:
//!
//! * `aws_s3.table_import_from_s3(...)` PARSES and RUNS on a PostgreSQL endpoint that has
//!   installed the extension: it reads the object its arguments name out of Amazon S3 and
//!   loads its records as rows of the table, so a `SELECT` afterwards returns exactly the
//!   object's rows, and it answers with the extension's own summary of what it imported. The
//!   form of the object is the call's own `COPY` options and its column list, and the object
//!   is named either by its bucket, key and Region or by the composite
//!   `aws_commons.create_s3_uri` builds.
//! * `aws_s3.query_export_to_s3(...)` runs the query it is given on the session and writes
//!   its rows into the object the call names, reporting the rows, files and bytes it
//!   uploaded — and the bytes it wrote are what Amazon S3's own `GetObject` then serves.
//! * WHETHER either function may touch the object is the instance's own state, not the
//!   call's: the engine acts under the IAM role the instance has ASSOCIATED for the feature
//!   the function needs (`s3Import` / `s3Export`), and only where that role's policies allow
//!   the object action. With no such association the call still runs and fails for the
//!   credentials it has not got; with a role whose policies state permissions that do not
//!   cover the action it fails with Amazon S3's access-denied error and loads nothing; with
//!   the association and the grant in place the identical call succeeds. A role that states
//!   no permissions at all is evidence of nothing and refuses nothing. The same permission
//!   decision gates the OTHER wire's Amazon S3 import (`LOAD DATA FROM S3`), which reports it
//!   in that server's own words.
//! * The functions come with the EXTENSION: a database that has not installed `aws_s3` has
//!   no such schema, which is what PostgreSQL reports about the call.
//! * `TRUNCATE TABLE` empties the table a client re-imports into: both engines define it, so
//!   the endpoint serves it and the identical import afterwards loads the object afresh.
//! * The IMPORT is a WRITE — it is the same bulk load a client's `COPY … FROM STDIN` performs
//!   — so a read-only transaction refuses it in PostgreSQL's own words and loads nothing,
//!   and the identical call is served once the session is writable again.
//!
//! Amazon S3 and IAM are mounted beside this service on ONE engine, exactly as the assembled
//! router mounts them: the objects an import reads were written through Amazon S3's own
//! `PutObject`, the object an export wrote is read back through its own `GetObject`, and the
//! roles are roles IAM holds, created with `CreateRole`/`PutRolePolicy`. The client is the
//! PostgreSQL protocol client the sibling endpoint suites drive, so what is exercised is what
//! a stock driver sends.

use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_parameter_groups::{create_group, modify};
use crate::db_instance_engine_endpoint::{
    endpoint_of, mysql_login, postgres_login, PostgresSession, Refusal, Transport, DATABASE,
    MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{call, expect_ok, text, REGION};
use crate::lifecycle_poll_loops::poll_until;
use crate::rds_s3_integration::{create_bucket, emulator, put_object, role, s3};

/// `ER_S3_API_ERROR` — the server error number the MySQL engines report a failure of their
/// own Amazon S3 call under, which is what the other wire's import refusal carries.
const ER_S3_API_ERROR: &str = "1871";

/// The bucket every object below lives in.
///
/// `pub(crate)` here and below so the sibling suite that drives the SAME endpoint under the
/// trust-policy half of the decision (`crate::postgres_s3_import_role_trust`) stages its
/// objects, stands its instance up and opens its session through these rather than growing a
/// second copy of them.
pub(crate) const BUCKET: &str = "emu-pg-aws-s3";

/// The service principal a role Amazon RDS acts under for a DB instance feature must trust.
const RDS_PRINCIPAL: &str = "rds.amazonaws.com";

/// An identity policy granting the object READ an import performs on `BUCKET`'s objects —
/// the `s3:GetObject` "Setting up access to an Amazon S3 bucket" asks for.
pub(crate) fn get_object_policy() -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":["s3:GetObject","s3:ListBucket"],"Resource":["arn:aws:s3:::{BUCKET}","arn:aws:s3:::{BUCKET}/*"]}}]}}"#
    )
}

/// An identity policy granting the object WRITE an export performs, and nothing else.
fn put_object_only_policy() -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"s3:PutObject","Resource":"arn:aws:s3:::{BUCKET}/*"}}]}}"#
    )
}

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// Create an RDS for PostgreSQL DB instance, poll `DescribeDBInstances` until it reports
/// `available`, and answer with the endpoint it published.
///
/// The wait is for the CONDITION — the status the service's own clock owes — through the one
/// poll loop this test target shares (`lifecycle_poll_loops::poll_until`).
pub(crate) async fn available_postgres(
    router: &MultiServiceRouter,
    identifier: &str,
) -> (String, u16) {
    let request: Vec<(&str, String)> = params![
        "DBInstanceIdentifier" => identifier,
        "Engine" => "postgres",
        "DBInstanceClass" => "db.t3.micro",
        "AllocatedStorage" => 20,
        "MasterUsername" => MASTER_USER,
        "MasterUserPassword" => MASTER_PASSWORD,
        "DBName" => DATABASE,
    ]
    .to_vec();
    let created = expect_ok(identifier, call(router, "CreateDBInstance", &request).await);
    let endpoint = endpoint_of(&created);
    let described = [("DBInstanceIdentifier", identifier)];
    poll_until(
        identifier,
        "the DB instance",
        || call(router, "DescribeDBInstances", &described),
        |status, body| status == 200 && body.contains("<DBInstanceStatus>available</DBInstanceStatus>"),
    )
    .await;
    endpoint
}

/// Associate `role_arn` with the instance for `feature`, and poll the report until the
/// association is `ACTIVE` — the state a client waits for before it uses the feature.
pub(crate) async fn associate(
    router: &MultiServiceRouter,
    identifier: &str,
    role_arn: &str,
    feature: &str,
) {
    expect_ok(
        feature,
        call(
            router,
            "AddRoleToDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "RoleArn" => role_arn,
                "FeatureName" => feature,
            ],
        )
        .await,
    );
    let described = [("DBInstanceIdentifier", identifier)];
    poll_until(
        feature,
        "the role association",
        || call(router, "DescribeDBInstances", &described),
        |status, body| status == 200 && body.contains(role_arn) && body.contains("<Status>ACTIVE</Status>"),
    )
    .await;
}

/// Drop the association of `role_arn` for `feature`.
pub(crate) async fn disassociate(
    router: &MultiServiceRouter,
    identifier: &str,
    role_arn: &str,
    feature: &str,
) {
    expect_ok(
        feature,
        call(
            router,
            "RemoveRoleFromDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "RoleArn" => role_arn,
                "FeatureName" => feature,
            ],
        )
        .await,
    );
}

/// A session on the instance's own endpoint, as its master user, with the extension
/// installed — which is what an S3-integrated instance's client does first.
pub(crate) async fn session_with_the_extension(address: &str, port: u16) -> PostgresSession {
    let mut session = postgres_login(
        address,
        port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| panic!("the master user must be admitted, got {refusal:?}"));
    run(&mut session, "the extension", "CREATE EXTENSION IF NOT EXISTS aws_s3 CASCADE").await;
    session
}

/// Run `sql`, failing the case when the engine refuses it.
pub(crate) async fn run(session: &mut PostgresSession, case: &str, sql: &str) {
    session
        .query(sql)
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: {sql:?} must run, got {refusal:?}"));
}

/// The refusal `sql` is answered with, failing the case when it is served instead.
pub(crate) async fn refusal(session: &mut PostgresSession, case: &str, sql: &str) -> Refusal {
    match session.query(sql).await {
        Ok(rows) => panic!("case {case:?}: {sql:?} must be refused, got {rows:?}"),
        Err(refusal) => refusal,
    }
}

/// The rows `table` holds, as text, in the order the engine returns them.
pub(crate) async fn rows_of(
    session: &mut PostgresSession,
    case: &str,
    table: &str,
) -> Vec<Vec<Option<String>>> {
    session
        .query(&format!("SELECT * FROM {table} ORDER BY 1"))
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: the read-back must run, got {refusal:?}"))
        .rows
}

/// One row of the import-format table: the object staged, the call that imports it, and the
/// rows the table must hold afterwards.
struct ImportRow {
    case: &'static str,
    /// The table's declaration, so each row spans the columns its own object carries.
    declaration: &'static str,
    /// The object's bytes.
    body: &'static str,
    /// The call, with `{table}`, `{bucket}`, `{key}` and `{region}` filled in from the row.
    statement: &'static str,
    /// The rows the table must hold once the import has run.
    expected: &'static [&'static [Option<&'static str>]],
}

/// The forms of the call and of the object this endpoint serves: the two `COPY` formats, a
/// header line, a delimiter of the call's own, a column list that names a subset, the
/// composite `s3_info` argument, a schema-qualified table name, a named `options` argument
/// and the text format's NULL marker.
///
/// Source for the signatures and the options: the `aws_s3.table_import_from_s3` reference
/// (Importing data from Amazon S3 into an RDS for PostgreSQL DB instance), whose `options`
/// are "arguments to the PostgreSQL COPY command", and `aws_commons.create_s3_uri`.
const IMPORT_ROWS: &[ImportRow] = &[
    ImportRow {
        case: "the csv format",
        declaration: "id integer, name text",
        body: "1,alpha\n2,beta\n",
        statement: "SELECT aws_s3.table_import_from_s3('{table}', '', '(format csv)', \
                    '{bucket}', '{key}', '{region}')",
        expected: &[&[Some("1"), Some("alpha")], &[Some("2"), Some("beta")]],
    },
    ImportRow {
        case: "the csv format with a header line",
        declaration: "id integer, name text",
        body: "id,name\n1,alpha\n2,beta\n",
        statement: "SELECT aws_s3.table_import_from_s3('{table}', '', '(format csv, header true)', \
                    '{bucket}', '{key}', '{region}')",
        expected: &[&[Some("1"), Some("alpha")], &[Some("2"), Some("beta")]],
    },
    ImportRow {
        case: "the text format, which is what a call naming no options reads",
        declaration: "id integer, name text",
        body: "1\talpha\n2\tbeta\n",
        statement: "SELECT aws_s3.table_import_from_s3('{table}', '', '', '{bucket}', '{key}', \
                    '{region}')",
        expected: &[&[Some("1"), Some("alpha")], &[Some("2"), Some("beta")]],
    },
    ImportRow {
        case: "a delimiter of the call's own, in a dollar-quoted argument",
        declaration: "id integer, name text",
        body: "1|alpha\n2|beta\n",
        statement: "SELECT aws_s3.table_import_from_s3('{table}', '', $$(format csv, delimiter '|')$$, \
                    '{bucket}', '{key}', '{region}')",
        expected: &[&[Some("1"), Some("alpha")], &[Some("2"), Some("beta")]],
    },
    ImportRow {
        case: "a column list naming a subset of the table's columns",
        declaration: "id integer, name text",
        body: "alpha\nbeta\n",
        statement: "SELECT aws_s3.table_import_from_s3('{table}', 'name', '(format csv)', \
                    '{bucket}', '{key}', '{region}')",
        expected: &[&[None, Some("alpha")], &[None, Some("beta")]],
    },
    ImportRow {
        case: "the object named by the composite aws_commons.create_s3_uri builds",
        declaration: "id integer, name text",
        body: "7,gamma\n",
        statement: "SELECT aws_s3.table_import_from_s3('{table}', '', '(format csv)', \
                    aws_commons.create_s3_uri('{bucket}', '{key}', '{region}'))",
        expected: &[&[Some("7"), Some("gamma")]],
    },
    ImportRow {
        case: "a schema-qualified table name",
        declaration: "id integer, name text",
        body: "3,delta\n",
        statement: "SELECT aws_s3.table_import_from_s3('public.{table}', '', '(format csv)', \
                    '{bucket}', '{key}', '{region}')",
        expected: &[&[Some("3"), Some("delta")]],
    },
    ImportRow {
        case: "the options named as an argument rather than placed",
        declaration: "id integer, name text",
        body: "4,epsilon\n",
        statement: "SELECT aws_s3.table_import_from_s3('{table}', '', options := '(format csv)', \
                    bucket := '{bucket}', file_path := '{key}', region := '{region}')",
        expected: &[&[Some("4"), Some("epsilon")]],
    },
    ImportRow {
        case: "the text format's NULL marker",
        declaration: "id integer, name text",
        body: "5\t\\N\n",
        statement: "SELECT aws_s3.table_import_from_s3('{table}', '', '(format text)', \
                    '{bucket}', '{key}', '{region}')",
        expected: &[&[Some("5"), None]],
    },
];

/// Rule 1: the import reads the object and loads its records as rows, in every form of the
/// call and of the object this endpoint serves, and answers with the extension's own summary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_aws_s3_import_loads_the_objects_records_as_rows() {
    let (router, _engine) = emulator();
    let identifier = "emu-pg-import";
    create_bucket(&router, BUCKET, REGION).await;
    let role_arn = role(
        &router,
        "emu-pg-import-role",
        RDS_PRINCIPAL,
        Some(get_object_policy()),
    )
    .await;
    let (address, port) = available_postgres(&router, identifier).await;
    associate(&router, identifier, &role_arn, "s3Import").await;
    let mut session = session_with_the_extension(&address, port).await;

    for (index, row) in IMPORT_ROWS.iter().enumerate() {
        let table = format!("landing_{index}");
        let key = format!("import/rows-{index}.csv");
        put_object(&router, BUCKET, &key, row.body.as_bytes().to_vec()).await;
        run(
            &mut session,
            row.case,
            &format!("CREATE TABLE {table} ({})", row.declaration),
        )
        .await;
        let statement = row
            .statement
            .replace("{table}", &table)
            .replace("{bucket}", BUCKET)
            .replace("{key}", &key)
            .replace("{region}", REGION);
        let answered = session
            .query(&statement)
            .await
            .unwrap_or_else(|refusal| panic!("case {:?}: {statement:?} must run, got {refusal:?}", row.case));
        let expected_summary = format!(
            "{} rows imported into relation \"{table}\" from file {key} of {} bytes",
            row.expected.len(),
            row.body.len()
        );
        assert_eq!(
            answered.only(),
            Some(expected_summary.as_str()),
            "case {:?}: expected the import summary {expected_summary:?}, got {answered:?}",
            row.case
        );
        let reported = rows_of(&mut session, row.case, &table).await;
        let expected: Vec<Vec<Option<String>>> = row
            .expected
            .iter()
            .map(|values| {
                values
                    .iter()
                    .map(|value| value.map(str::to_string))
                    .collect()
            })
            .collect();
        assert_eq!(
            reported, expected,
            "case {:?}: expected the object's rows in the table, got {reported:?}",
            row.case
        );
    }
}

/// Rule 2: the capability is the instance's own state. The same call on the same object is
/// refused for the credentials it has not got until the role is associated for the FEATURE it
/// needs, refused with Amazon S3's access-denied error while that role's policies do not
/// cover the read, and served the moment both halves are in place — and refused again once
/// the association is dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whether_the_import_may_read_the_object_follows_the_instances_role_association() {
    let (router, _engine) = emulator();
    let identifier = "emu-pg-capability";
    create_bucket(&router, BUCKET, REGION).await;
    let key = "import/capability.csv";
    put_object(&router, BUCKET, key, b"1,alpha\n".to_vec()).await;
    // Two roles IAM holds: one whose policies cover the object read, and one whose policies
    // state permissions that do not.
    let reader = role(
        &router,
        "emu-pg-reader",
        RDS_PRINCIPAL,
        Some(get_object_policy()),
    )
    .await;
    let writer_only = role(
        &router,
        "emu-pg-writer-only",
        RDS_PRINCIPAL,
        Some(put_object_only_policy()),
    )
    .await;
    let (address, port) = available_postgres(&router, identifier).await;
    let mut session = session_with_the_extension(&address, port).await;
    run(&mut session, "the table", "CREATE TABLE landing (id integer, name text)").await;
    let import = format!(
        "SELECT aws_s3.table_import_from_s3('landing', '', '(format csv)', '{BUCKET}', '{key}', \
         '{REGION}')"
    );

    // No association at all: the extension has no credentials for the feature.
    let answered = refusal(&mut session, "no association", &import).await;
    assert_eq!(
        answered.message, "credentials for the s3import feature could not be accessed",
        "case {:?}: expected the extension's credentials error, got {answered:?}",
        "no association"
    );

    // An association for ANOTHER feature is none for this one.
    associate(&router, identifier, &reader, "s3Export").await;
    let answered = refusal(&mut session, "associated for another feature", &import).await;
    assert_eq!(
        answered.message, "credentials for the s3import feature could not be accessed",
        "case {:?}: expected the extension's credentials error, got {answered:?}",
        "associated for another feature"
    );
    disassociate(&router, identifier, &reader, "s3Export").await;

    // A role whose policies state permissions that do not cover the read: Amazon S3's own
    // access-denied error, and nothing is loaded.
    associate(&router, identifier, &writer_only, "s3Import").await;
    let answered = refusal(&mut session, "a role without s3:GetObject", &import).await;
    assert!(
        answered.message.to_lowercase().contains("accessdenied"),
        "case {:?}: expected Amazon S3's access-denied error, got {answered:?}",
        "a role without s3:GetObject"
    );
    assert!(
        answered.message.contains("s3:GetObject")
            && answered
                .message
                .contains(&format!("arn:aws:s3:::{BUCKET}/{key}")),
        "case {:?}: the refusal must name the action and the object, got {answered:?}",
        "a role without s3:GetObject"
    );
    let reported = rows_of(&mut session, "a role without s3:GetObject", "landing").await;
    assert!(
        reported.is_empty(),
        "case {:?}: a refused import loads nothing, got {reported:?}",
        "a role without s3:GetObject"
    );
    disassociate(&router, identifier, &writer_only, "s3Import").await;

    // Both halves in place: the identical call is served.
    associate(&router, identifier, &reader, "s3Import").await;
    run(&mut session, "the role that may read", &import).await;
    let reported = rows_of(&mut session, "the role that may read", "landing").await;
    assert_eq!(
        reported,
        vec![vec![Some("1".to_string()), Some("alpha".to_string())]],
        "case {:?}: expected the object's row in the table, got {reported:?}",
        "the role that may read"
    );

    // Emptying the table and importing again loads the same object afresh, which is what a
    // client that re-imports does: `TRUNCATE` removes every row, and the identical call puts
    // the object's rows back.
    run(&mut session, "emptying the table", "TRUNCATE TABLE landing").await;
    let reported = rows_of(&mut session, "emptying the table", "landing").await;
    assert!(
        reported.is_empty(),
        "case {:?}: TRUNCATE removes every row, got {reported:?}",
        "emptying the table"
    );
    run(&mut session, "the re-import", &import).await;
    let reported = rows_of(&mut session, "the re-import", "landing").await;
    assert_eq!(
        reported,
        vec![vec![Some("1".to_string()), Some("alpha".to_string())]],
        "case {:?}: expected the object's row again, got {reported:?}",
        "the re-import"
    );

    // And dropped again: the engine can no longer act under it.
    disassociate(&router, identifier, &reader, "s3Import").await;
    let answered = refusal(&mut session, "the association dropped", &import).await;
    assert_eq!(
        answered.message, "credentials for the s3import feature could not be accessed",
        "case {:?}: expected the extension's credentials error, got {answered:?}",
        "the association dropped"
    );
}

/// Rule 3: an object the store does not hold is the Amazon S3 error for that class, and a
/// role that states NO permissions is evidence of nothing — the call is served under it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_object_the_store_has_not_got_is_the_amazon_s3_error_for_it() {
    let (router, _engine) = emulator();
    let identifier = "emu-pg-absent";
    create_bucket(&router, BUCKET, REGION).await;
    put_object(&router, BUCKET, "import/present.csv", b"1,alpha\n".to_vec()).await;
    // A role carrying no identity policy at all: a role provisioned out of band routinely
    // carries none, so nothing is known about what it may do and nothing is refused for it.
    let role_arn = role(&router, "emu-pg-absent-role", RDS_PRINCIPAL, None).await;
    let (address, port) = available_postgres(&router, identifier).await;
    associate(&router, identifier, &role_arn, "s3Import").await;
    let mut session = session_with_the_extension(&address, port).await;
    run(&mut session, "the table", "CREATE TABLE landing (id integer, name text)").await;

    // (case, the bucket the call names, the key it names, the message it must be refused with)
    let rows: Vec<(&str, &str, &str, &str)> = vec![
        (
            "a key the bucket has not got",
            BUCKET,
            "import/absent.csv",
            "S3 API returned error: NoSuchKey: The specified key does not exist",
        ),
        (
            "a bucket Amazon S3 does not hold",
            "emu-pg-no-such-bucket",
            "import/present.csv",
            "S3 API returned error: NoSuchBucket: The specified bucket does not exist",
        ),
    ];
    for (case, bucket, key, expected) in rows {
        let answered = refusal(
            &mut session,
            case,
            &format!(
                "SELECT aws_s3.table_import_from_s3('landing', '', '(format csv)', '{bucket}', \
                 '{key}', '{REGION}')"
            ),
        )
        .await;
        assert_eq!(
            answered.message, expected,
            "case {case:?}: expected {expected:?}, got {answered:?}"
        );
    }

    // The role that states no permissions still imports: the object it names is there.
    run(
        &mut session,
        "a role that states no permissions",
        &format!(
            "SELECT aws_s3.table_import_from_s3('landing', '', '(format csv)', '{BUCKET}', \
             'import/present.csv', '{REGION}')"
        ),
    )
    .await;
    let reported = rows_of(&mut session, "a role that states no permissions", "landing").await;
    assert_eq!(
        reported,
        vec![vec![Some("1".to_string()), Some("alpha".to_string())]],
        "expected the object's row in the table, got {reported:?}"
    );
}

/// One row of the export table: the options the call names and the bytes the object must then
/// hold.
struct ExportRow {
    case: &'static str,
    /// The call, with `{bucket}`, `{key}` and `{region}` filled in from the row.
    statement: &'static str,
    /// The bytes the exported object must carry.
    expected: &'static str,
}

/// The export's two formats and its header line, over the same two rows.
const EXPORT_ROWS: &[ExportRow] = &[
    ExportRow {
        case: "the csv format",
        statement: "SELECT * FROM aws_s3.query_export_to_s3('SELECT id, name FROM exported ORDER BY id', \
                    '{bucket}', '{key}', '{region}', options := '(format csv)')",
        expected: "1,alpha\n2,beta\n",
    },
    ExportRow {
        case: "the csv format with a header line",
        statement: "SELECT * FROM aws_s3.query_export_to_s3('SELECT id, name FROM exported ORDER BY id', \
                    '{bucket}', '{key}', '{region}', options := '(format csv, header true)')",
        expected: "id,name\n1,alpha\n2,beta\n",
    },
    ExportRow {
        case: "the text format, which is what a call naming no options writes",
        statement: "SELECT * FROM aws_s3.query_export_to_s3('SELECT id, name FROM exported ORDER BY id', \
                    '{bucket}', '{key}', '{region}')",
        expected: "1\talpha\n2\tbeta\n",
    },
    ExportRow {
        case: "the object named by the composite aws_commons.create_s3_uri builds",
        statement: "SELECT * FROM aws_s3.query_export_to_s3('SELECT id, name FROM exported ORDER BY id', \
                    aws_commons.create_s3_uri('{bucket}', '{key}', '{region}'), '(format csv)')",
        expected: "1,alpha\n2,beta\n",
    },
];

/// Rule 4: the export runs the query and writes its rows into the object, reporting what it
/// uploaded — and the bytes it wrote are the bytes Amazon S3 then serves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_aws_s3_export_writes_the_querys_rows_into_the_object() {
    let (router, _engine) = emulator();
    let identifier = "emu-pg-export";
    create_bucket(&router, BUCKET, REGION).await;
    let role_arn = role(
        &router,
        "emu-pg-export-role",
        RDS_PRINCIPAL,
        Some(put_object_only_policy()),
    )
    .await;
    let (address, port) = available_postgres(&router, identifier).await;
    associate(&router, identifier, &role_arn, "s3Export").await;
    let mut session = session_with_the_extension(&address, port).await;
    run(
        &mut session,
        "the exported table",
        "CREATE TABLE exported (id integer, name text)",
    )
    .await;
    run(
        &mut session,
        "the exported rows",
        "INSERT INTO exported (id, name) VALUES (1, 'alpha'), (2, 'beta')",
    )
    .await;

    for (index, row) in EXPORT_ROWS.iter().enumerate() {
        let key = format!("export/rows-{index}.csv");
        let statement = row
            .statement
            .replace("{bucket}", BUCKET)
            .replace("{key}", &key)
            .replace("{region}", REGION);
        let answered = session
            .query(&statement)
            .await
            .unwrap_or_else(|refusal| panic!("case {:?}: {statement:?} must run, got {refusal:?}", row.case));
        assert_eq!(
            answered.columns,
            vec![
                "rows_uploaded".to_string(),
                "files_uploaded".to_string(),
                "bytes_uploaded".to_string(),
            ],
            "case {:?}: expected the record the export answers with, got {answered:?}",
            row.case
        );
        assert_eq!(
            answered.rows,
            vec![vec![
                Some("2".to_string()),
                Some("1".to_string()),
                Some(row.expected.len().to_string()),
            ]],
            "case {:?}: expected the rows, files and bytes uploaded, got {answered:?}",
            row.case
        );
        let stored = s3(&router)
            .get_object()
            .bucket(BUCKET)
            .key(&key)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {:?}: Amazon S3 holds the exported object: {error}", row.case)
            })
            .body
            .collect()
            .await
            .expect("the exported object's bytes")
            .to_vec();
        assert_eq!(
            String::from_utf8_lossy(&stored),
            row.expected,
            "case {:?}: expected the exported bytes, got {:?}",
            row.case,
            String::from_utf8_lossy(&stored)
        );
    }
}

/// Rule 5: the export is gated the same way the import is — on the `s3Export` association and
/// on the role's own permission to write the object — and a refused export writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whether_the_export_may_write_the_object_follows_the_instances_role_association() {
    let (router, _engine) = emulator();
    let identifier = "emu-pg-export-gate";
    create_bucket(&router, BUCKET, REGION).await;
    let reader_only = role(
        &router,
        "emu-pg-export-reader-only",
        RDS_PRINCIPAL,
        Some(get_object_policy()),
    )
    .await;
    let (address, port) = available_postgres(&router, identifier).await;
    let mut session = session_with_the_extension(&address, port).await;
    run(
        &mut session,
        "the exported table",
        "CREATE TABLE exported (id integer, name text)",
    )
    .await;
    run(
        &mut session,
        "the exported rows",
        "INSERT INTO exported (id, name) VALUES (1, 'alpha')",
    )
    .await;
    let key = "export/gated.csv";
    let export = format!(
        "SELECT * FROM aws_s3.query_export_to_s3('SELECT id, name FROM exported', '{BUCKET}', \
         '{key}', '{REGION}', options := '(format csv)')"
    );

    let answered = refusal(&mut session, "no association", &export).await;
    assert_eq!(
        answered.message, "credentials for the s3export feature could not be accessed",
        "case {:?}: expected the extension's credentials error, got {answered:?}",
        "no association"
    );

    associate(&router, identifier, &reader_only, "s3Export").await;
    let answered = refusal(&mut session, "a role without s3:PutObject", &export).await;
    assert!(
        answered.message.to_lowercase().contains("accessdenied")
            && answered.message.contains("s3:PutObject"),
        "case {:?}: expected Amazon S3's access-denied error naming the write, got {answered:?}",
        "a role without s3:PutObject"
    );
    // A refused export wrote nothing: Amazon S3 holds no such object.
    let read = s3(&router)
        .get_object()
        .bucket(BUCKET)
        .key(key)
        .send()
        .await;
    assert!(
        read.is_err(),
        "case {:?}: a refused export writes no object, got {read:?}",
        "a role without s3:PutObject"
    );
}

/// Rule 6: the functions come with the extension — a database that has not installed `aws_s3`
/// has no such schema, which is what PostgreSQL reports about the call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_functions_answer_only_where_the_extension_is_installed() {
    let (router, _engine) = emulator();
    let identifier = "emu-pg-no-extension";
    create_bucket(&router, BUCKET, REGION).await;
    put_object(&router, BUCKET, "import/rows.csv", b"1,alpha\n".to_vec()).await;
    let role_arn = role(
        &router,
        "emu-pg-no-extension-role",
        RDS_PRINCIPAL,
        Some(get_object_policy()),
    )
    .await;
    let (address, port) = available_postgres(&router, identifier).await;
    associate(&router, identifier, &role_arn, "s3Import").await;
    let mut session = postgres_login(
        &address,
        port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| panic!("the master user must be admitted, got {refusal:?}"));
    run(
        &mut session,
        "the table",
        "CREATE TABLE landing (id integer, name text)",
    )
    .await;

    let import = format!(
        "SELECT aws_s3.table_import_from_s3('landing', '', '(format csv)', '{BUCKET}', \
         'import/rows.csv', '{REGION}')"
    );
    let answered = refusal(&mut session, "the extension not installed", &import).await;
    assert_eq!(
        (answered.code.as_str(), answered.message.as_str()),
        ("3F000", "schema \"aws_s3\" does not exist"),
        "case {:?}: expected PostgreSQL's missing-schema condition, got {answered:?}",
        "the extension not installed"
    );

    // Installed, the identical call is served from the same session.
    run(
        &mut session,
        "the extension",
        "CREATE EXTENSION aws_s3 CASCADE",
    )
    .await;
    run(&mut session, "the extension installed", &import).await;
    let reported = rows_of(&mut session, "the extension installed", "landing").await;
    assert_eq!(
        reported,
        vec![vec![Some("1".to_string()), Some("alpha".to_string())]],
        "expected the object's row in the table, got {reported:?}"
    );

    // And the IMPORT is a write, so a read-only transaction refuses it in PostgreSQL's own
    // words, loads nothing, and the identical call is served again once the session is
    // writable.
    run(
        &mut session,
        "the read-only session",
        "SET default_transaction_read_only = on",
    )
    .await;
    let answered = refusal(&mut session, "a read-only transaction", &import).await;
    assert_eq!(
        (answered.code.as_str(), answered.message.as_str()),
        (
            "25006",
            "cannot execute COPY FROM in a read-only transaction"
        ),
        "case {:?}: expected PostgreSQL's read-only condition, got {answered:?}",
        "a read-only transaction"
    );
    let reported = rows_of(&mut session, "a read-only transaction", "landing").await;
    assert_eq!(
        reported.len(),
        1,
        "case {:?}: a refused import loads nothing, got {reported:?}",
        "a read-only transaction"
    );
    run(
        &mut session,
        "the session writable again",
        "SET default_transaction_read_only = off",
    )
    .await;
    run(&mut session, "the import once writable", &import).await;
    let reported = rows_of(&mut session, "the import once writable", "landing").await;
    assert_eq!(
        reported.len(),
        2,
        "case {:?}: expected the object's row loaded a second time, got {reported:?}",
        "the import once writable"
    );
}

/// Rule 7: the role's permission to touch the object gates the OTHER wire's Amazon S3 import
/// in the same way — `LOAD DATA FROM S3` on a MySQL endpoint fails with Amazon S3's
/// access-denied error, under that server's own error number for a failed S3 call, when the
/// policies in force on the role the engine loads under do not cover the object read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_permission_gates_the_mysql_wires_s3_import() {
    let (router, _engine) = emulator();
    let identifier = "emu-mysql-import-gate";
    create_bucket(&router, BUCKET, REGION).await;
    let key = "import/mysql.csv";
    put_object(&router, BUCKET, key, b"1,alpha\n".to_vec()).await;
    // The role the engine loads under: associated with the cluster and named by the
    // import-role parameter, which is what an Aurora MySQL S3 import is configured with —
    // and carrying policies that cover the object WRITE and not the read.
    let role_arn = role(
        &router,
        "emu-mysql-writer-only",
        RDS_PRINCIPAL,
        Some(put_object_only_policy()),
    )
    .await;
    let group = create_group(&router, identifier, "emu-mysql-gate-cpg", "aurora-mysql8.0").await;
    expect_ok(
        identifier,
        modify(&router, &group, "aws_default_s3_role", &role_arn, None).await,
    );
    let (address, port) = available_aurora_mysql(&router, identifier, &group).await;
    expect_ok(
        identifier,
        call(
            &router,
            "AddRoleToDBCluster",
            &params![
                "DBClusterIdentifier" => identifier,
                "RoleArn" => role_arn,
            ],
        )
        .await,
    );
    let mut session = mysql_login(
        &address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| panic!("the master user must be admitted, got {refusal:?}"));
    session
        .query("CREATE TABLE landing (id int, name varchar(32))")
        .await
        .unwrap_or_else(|refusal| panic!("the table must be created, got {refusal:?}"));

    let statement = format!(
        "LOAD DATA FROM S3 's3://{BUCKET}/{key}' INTO TABLE landing FIELDS TERMINATED BY ','"
    );
    let answered = session
        .query(&statement)
        .await
        .expect_err("a role without the object read must be refused");
    assert_eq!(
        answered.code, ER_S3_API_ERROR,
        "expected the server's S3 API error number, got {answered:?}"
    );
    assert!(
        answered.message.to_lowercase().contains("accessdenied")
            && answered.message.contains("s3:GetObject"),
        "expected Amazon S3's access-denied error naming the read, got {answered:?}"
    );
    let loaded = session
        .query("SELECT * FROM landing")
        .await
        .unwrap_or_else(|refusal| panic!("the read-back must run, got {refusal:?}"))
        .rows;
    assert!(
        loaded.is_empty(),
        "a refused load leaves no row, got {loaded:?}"
    );
}

/// Create an Aurora MySQL DB cluster with `group` as its cluster parameter group, poll
/// `DescribeDBClusters` until it reports `available`, and answer with the endpoint it
/// published.
async fn available_aurora_mysql(
    router: &MultiServiceRouter,
    identifier: &str,
    group: &str,
) -> (String, u16) {
    let request: Vec<(&str, String)> = params![
        "DBClusterIdentifier" => identifier,
        "Engine" => "aurora-mysql",
        "MasterUsername" => MASTER_USER,
        "MasterUserPassword" => MASTER_PASSWORD,
        "DatabaseName" => DATABASE,
        "DBClusterParameterGroupName" => group,
    ]
    .to_vec();
    let created = expect_ok(identifier, call(router, "CreateDBCluster", &request).await);
    let address = text(&created, "Endpoint");
    let port: u16 = text(&created, "Port")
        .parse()
        .unwrap_or_else(|_| panic!("case {identifier:?}: the cluster publishes a port"));
    let described = [("DBClusterIdentifier", identifier)];
    poll_until(
        identifier,
        "the DB cluster",
        || call(router, "DescribeDBClusters", &described),
        |status, body| status == 200 && body.contains("<Status>available</Status>"),
    )
    .await;
    (address, port)
}
