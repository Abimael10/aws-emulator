// SPDX-License-Identifier: CC0-1.0

//! WHETHER the `aws_s3` extension's statement on a PostgreSQL endpoint may touch the object
//! is decided by the permissions IN FORCE on the IAM role the DB instance has associated for
//! the feature — never by the association itself.
//!
//! The rule these pin, in two halves:
//!
//! * the ASSOCIATION is a record, not a decision: `AddRoleToDBInstance` records the role for
//!   the feature it names and `DBInstance$AssociatedRoles` reports it `ACTIVE` whatever that
//!   role's permission policies allow, so a client reaches the state it polls for with a role
//!   that may read the object and with one that may not;
//! * the OBJECT operation is then performed only where the permissions in force on that role
//!   allow it, read in IAM's own order: an explicit `Deny` refuses, a grant stands only where
//!   the permissions BOUNDARY attached to the role admits the same call, a policy set that
//!   states permissions none of which covers the call withholds it, and a role that states no
//!   permissions at all is evidence of nothing. A withheld call fails with Amazon S3's own
//!   access-denied error, under PostgreSQL's SQLSTATE for an insufficient privilege, and moves
//!   no data — no row is loaded by a refused import, and no object is written by a refused
//!   export.
//!
//! The rows span what the rule generalises over: the shape of the GRANT (every object of the
//! bucket, the one object by name, the service wildcard, another bucket's objects, the bucket
//! itself rather than its objects, no policy at all, a grant with an explicit deny beside it)
//! and, independently, the CEILING in force over it (none, the same call, the whole service,
//! only another action, a deny of the call, another bucket's objects) — and the capability,
//! since the same decision gates the import's read and the export's write under each one's own
//! object action.
//!
//! The permissions boundary is the `s3:GetObject` ceiling of the issue this suite was written
//! for: a boundary is attached as a MANAGED policy ("the ARN of the managed policy that is
//! used to set the permissions boundary"), so each ceiling here is a policy IAM holds, created
//! with `CreatePolicy` and attached with `PutRolePermissionsBoundary` and read back off the
//! role before the call it caps is made.
//!
//! Amazon S3 and IAM are mounted beside this service on ONE engine, exactly as the assembled
//! router mounts them: the object an import reads was written through Amazon S3's own
//! `PutObject`, the object an export wrote is read back through its own `GetObject`, and the
//! roles, their policies and their boundaries are what that service's own operations stored.
//! The client is the PostgreSQL protocol client the sibling endpoint suites drive, so what is
//! exercised is what a stock driver sends.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_engine_endpoint::{
    available_instance, master_session, must_run, PostgresSession,
};
use crate::db_instances::{call, expect_ok, REGION};
use crate::lifecycle_poll_loops::poll_until;
use crate::rds_s3_integration::{
    associated_roles, create_bucket, emulator, iam, put_object, role, s3,
};

/// The bucket every object below lives in.
const BUCKET: &str = "emu-pg-ceiling";

/// A SECOND bucket, named only by the grants that cover the wrong one.
const OTHER_BUCKET: &str = "emu-pg-ceiling-elsewhere";

/// The object an import reads, and its bytes.
const KEY: &str = "import/ceiling.csv";
const OBJECT_BODY: &[u8] = b"1,alpha\n";

/// The service principal a role Amazon RDS acts under for a DB instance feature must trust.
const RDS_PRINCIPAL: &str = "rds.amazonaws.com";

/// The `FeatureName` each capability is associated under — the names the PostgreSQL engine
/// families publish in `SupportedFeatureNames`.
const IMPORT_FEATURE: &str = "s3Import";
const EXPORT_FEATURE: &str = "s3Export";

/// The status an association that holds reports.
const ASSOCIATION_ACTIVE: &str = "ACTIVE";

/// PostgreSQL's SQLSTATE for `insufficient_privilege`, which is what a client branches a
/// refused `aws_s3` call on. Spelled out here rather than imported, so a wrong constant in
/// the implementation cannot make this suite pass.
const INSUFFICIENT_PRIVILEGE: &str = "42501";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

// ---------------------------------------------------------------------------
// The policy documents the rows put in force. `{bucket}`, `{key}` and `{other}` are filled
// in by `document` below, so one template reads for the grant a role carries and for the
// ceiling attached over it.
// ---------------------------------------------------------------------------

/// The grant the user guide's import role carries: "s3:GetObject" and "s3:ListBucket" on the
/// bucket and on the objects in it ("Setting up access to an Amazon S3 bucket", Amazon RDS
/// User Guide).
const READ_EVERY_OBJECT: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:GetObject","s3:ListBucket"],"Resource":["arn:aws:s3:::{bucket}","arn:aws:s3:::{bucket}/*"]}]}"#;

/// The same grant narrowed to the ONE object the call names.
const READ_THE_ONE_OBJECT: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::{bucket}/{key}"}]}"#;

/// A grant over the whole service, which covers every object action.
const EVERY_S3_ACTION: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:*","Resource":"*"}]}"#;

/// A grant of the object WRITE an export performs, and nothing else.
const WRITE_EVERY_OBJECT: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:PutObject","Resource":"arn:aws:s3:::{bucket}/*"}]}"#;

/// The read granted on ANOTHER bucket's objects: permissions stated, none of them about this
/// object.
const READ_ANOTHER_BUCKETS_OBJECTS: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::{other}/*"}]}"#;

/// The read granted on the BUCKET and not on the objects in it, which are two resources.
const READ_THE_BUCKET_ITSELF: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::{bucket}"}]}"#;

/// Every S3 action granted with the object READ explicitly denied beside it.
const EVERY_ACTION_BUT_THE_READ: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:*","Resource":"*"},{"Effect":"Deny","Action":"s3:GetObject","Resource":"arn:aws:s3:::{bucket}/*"}]}"#;

/// And with the object WRITE explicitly denied beside it.
const EVERY_ACTION_BUT_THE_WRITE: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:*","Resource":"*"},{"Effect":"Deny","Action":"s3:PutObject","Resource":"arn:aws:s3:::{bucket}/*"}]}"#;

/// One template with this suite's bucket, object and second bucket in it.
fn document(template: &str) -> String {
    template
        .replace("{bucket}", BUCKET)
        .replace("{key}", KEY)
        .replace("{other}", OTHER_BUCKET)
}

/// One row: the permissions in force on the role the engine acts under, and whether the
/// statement they gate is served.
struct Row {
    case: &'static str,
    /// The identity-based policy the role carries inline; `None` for a role that states no
    /// permissions at all.
    policy: Option<&'static str>,
    /// The document of the managed policy attached as the role's permissions boundary;
    /// `None` for a role carrying no ceiling.
    ceiling: Option<&'static str>,
    /// Whether the object operation is performed.
    served: bool,
}

/// The IMPORT's object read, under each shape of grant and each ceiling over it.
const IMPORT_ROWS: &[Row] = &[
    Row {
        case: "a grant of the object read and no ceiling",
        policy: Some(READ_EVERY_OBJECT),
        ceiling: None,
        served: true,
    },
    Row {
        case: "a grant naming the one object the call reads",
        policy: Some(READ_THE_ONE_OBJECT),
        ceiling: None,
        served: true,
    },
    Row {
        case: "a grant over the whole service",
        policy: Some(EVERY_S3_ACTION),
        ceiling: None,
        served: true,
    },
    Row {
        case: "no permissions stated at all, which is evidence of nothing",
        policy: None,
        ceiling: None,
        served: true,
    },
    Row {
        case: "permissions stated that do not cover the read",
        policy: Some(WRITE_EVERY_OBJECT),
        ceiling: None,
        served: false,
    },
    Row {
        case: "the read granted on another bucket's objects",
        policy: Some(READ_ANOTHER_BUCKETS_OBJECTS),
        ceiling: None,
        served: false,
    },
    Row {
        case: "the read granted on the bucket and not on its objects",
        policy: Some(READ_THE_BUCKET_ITSELF),
        ceiling: None,
        served: false,
    },
    Row {
        case: "an explicit deny of the read beside a grant over the service",
        policy: Some(EVERY_ACTION_BUT_THE_READ),
        ceiling: None,
        served: false,
    },
    Row {
        case: "a grant of the read and a ceiling that admits the same read",
        policy: Some(READ_EVERY_OBJECT),
        ceiling: Some(READ_EVERY_OBJECT),
        served: true,
    },
    Row {
        case: "a grant of the read and a ceiling over the whole service",
        policy: Some(READ_EVERY_OBJECT),
        ceiling: Some(EVERY_S3_ACTION),
        served: true,
    },
    Row {
        case: "a grant of the read and a ceiling that admits only another action",
        policy: Some(READ_EVERY_OBJECT),
        ceiling: Some(WRITE_EVERY_OBJECT),
        served: false,
    },
    Row {
        case: "a grant of the read and a ceiling that denies it",
        policy: Some(READ_EVERY_OBJECT),
        ceiling: Some(EVERY_ACTION_BUT_THE_READ),
        served: false,
    },
    Row {
        case: "a grant of the read and a ceiling over another bucket's objects",
        policy: Some(READ_EVERY_OBJECT),
        ceiling: Some(READ_ANOTHER_BUCKETS_OBJECTS),
        served: false,
    },
];

/// The EXPORT's object write, gated by the same decision under its own object action.
const EXPORT_ROWS: &[Row] = &[
    Row {
        case: "a grant of the object write and a ceiling that admits the same write",
        policy: Some(WRITE_EVERY_OBJECT),
        ceiling: Some(WRITE_EVERY_OBJECT),
        served: true,
    },
    Row {
        case: "a grant of the object write and a ceiling that admits only the read",
        policy: Some(WRITE_EVERY_OBJECT),
        ceiling: Some(READ_EVERY_OBJECT),
        served: false,
    },
    Row {
        case: "an explicit deny of the write beside a grant over the service",
        policy: Some(EVERY_ACTION_BUT_THE_WRITE),
        ceiling: None,
        served: false,
    },
];

/// Create the role `name` trusting Amazon RDS, carrying `policy` inline where the row states
/// one, with `ceiling` attached as its permissions boundary where the row states one — and
/// answer with the role's ARN.
///
/// A boundary is a MANAGED policy, so the ceiling is created with `CreatePolicy` and attached
/// with `PutRolePermissionsBoundary`; the attachment is read back off the role, so a row that
/// expects a ceiling is one whose role really carries it.
async fn role_under_a_ceiling(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    policy: Option<&str>,
    ceiling: Option<&str>,
) -> String {
    let role_arn = role(router, name, RDS_PRINCIPAL, policy.map(document)).await;
    if let Some(ceiling) = ceiling {
        let created = iam(router)
            .create_policy()
            .policy_name(format!("{name}-ceiling"))
            .policy_document(document(ceiling))
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: IAM creates the ceiling policy: {error}")
            });
        let ceiling_arn = created
            .policy()
            .and_then(|policy| policy.arn())
            .unwrap_or_else(|| panic!("case {case:?}: the created policy reports its ARN"))
            .to_string();
        iam(router)
            .put_role_permissions_boundary()
            .role_name(name)
            .permissions_boundary(&ceiling_arn)
            .send()
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: IAM attaches the ceiling: {error}"));
        let read = iam(router)
            .get_role()
            .role_name(name)
            .send()
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: IAM reports the role: {error}"));
        let attached = read
            .role()
            .and_then(|role| role.permissions_boundary())
            .and_then(|boundary| boundary.permissions_boundary_arn())
            .map(str::to_string);
        assert_eq!(
            attached,
            Some(ceiling_arn),
            "case {case:?}: the role must report the ceiling that was attached, got \
             {attached:?}"
        );
    }
    role_arn
}

/// Associate `role_arn` for `feature` and wait until the report reads `ACTIVE`, asserting the
/// triple `AssociatedRoles` carries — the record a client polls for, which is the same
/// whatever the role's permission policies allow.
async fn associate_active(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    role_arn: &str,
    feature: &str,
) {
    expect_ok(
        case,
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
    let active = format!("<Status>{ASSOCIATION_ACTIVE}</Status>");
    let (reported, _polls) = poll_until(
        case,
        "the role association",
        || call(router, "DescribeDBInstances", &described),
        |status, body| status == 200 && body.contains(&active),
    )
    .await;
    let roles = associated_roles(&reported);
    assert_eq!(
        roles,
        vec![(
            role_arn.to_string(),
            feature.to_string(),
            ASSOCIATION_ACTIVE.to_string()
        )],
        "case {case:?}: expected the one association reported ACTIVE, got {roles:?}"
    );
}

/// Drop the association again, so the next row's role is the only one the feature has.
async fn disassociate(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    role_arn: &str,
    feature: &str,
) {
    expect_ok(
        case,
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

/// The rows `table` holds, in the order the engine returns them.
async fn rows_of(
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

/// Rule: the association reads `ACTIVE` under every one of these roles, and the import's
/// object read is performed only where the permissions in force on the associated role allow
/// it — a withheld read is Amazon S3's access-denied error and loads nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_permissions_in_force_on_the_associated_role_decide_the_imports_object_read() {
    let (router, _engine) = emulator();
    let identifier = "emu-pg-ceiling-import";
    create_bucket(&router, BUCKET, REGION).await;
    put_object(&router, BUCKET, KEY, OBJECT_BODY.to_vec()).await;
    let (address, port) = available_instance(&router, identifier, "postgres", &[]).await;
    let mut session = master_session("the import's session", &address, port).await;
    must_run(
        "the extension",
        &mut session,
        "CREATE EXTENSION IF NOT EXISTS aws_s3 CASCADE",
    )
    .await;
    must_run(
        "the landing table",
        &mut session,
        "CREATE TABLE landing (id integer, name text)",
    )
    .await;
    let import = format!(
        "SELECT aws_s3.table_import_from_s3('landing', '', '(format csv)', '{BUCKET}', '{KEY}', \
         '{REGION}')"
    );
    // The sentence the extension answers a read its role may not perform with: the feature
    // whose credentials could not be used, and the Amazon S3 error the engine's own call was
    // refused with, naming the action and the object.
    let withheld = format!(
        "credentials for the s3import feature could not be accessed: S3 API returned error: \
         AccessDenied: Access Denied (s3:GetObject on arn:aws:s3:::{BUCKET}/{KEY})"
    );

    for (index, row) in IMPORT_ROWS.iter().enumerate() {
        let name = format!("emu-ceiling-import-{index}");
        let role_arn =
            role_under_a_ceiling(&router, row.case, &name, row.policy, row.ceiling).await;
        associate_active(&router, row.case, identifier, &role_arn, IMPORT_FEATURE).await;
        must_run(row.case, &mut session, "TRUNCATE TABLE landing").await;
        let answered = session.query(&import).await;
        if row.served {
            answered.unwrap_or_else(|refusal| {
                panic!(
                    "case {:?}: the import must be served, got {refusal:?}",
                    row.case
                )
            });
            let reported = rows_of(&mut session, row.case, "landing").await;
            assert_eq!(
                reported,
                vec![vec![Some("1".to_string()), Some("alpha".to_string())]],
                "case {:?}: expected the object's row in the table, got {reported:?}",
                row.case
            );
        } else {
            let refusal = match answered {
                Ok(rows) => panic!(
                    "case {:?}: the import must be refused, got {rows:?}",
                    row.case
                ),
                Err(refusal) => refusal,
            };
            assert_eq!(
                refusal.message, withheld,
                "case {:?}: expected Amazon S3's access-denied error, got {refusal:?}",
                row.case
            );
            assert_eq!(
                refusal.code, INSUFFICIENT_PRIVILEGE,
                "case {:?}: expected the insufficient-privilege SQLSTATE, got {refusal:?}",
                row.case
            );
            let reported = rows_of(&mut session, row.case, "landing").await;
            assert!(
                reported.is_empty(),
                "case {:?}: a refused import loads nothing, got {reported:?}",
                row.case
            );
        }
        disassociate(&router, row.case, identifier, &role_arn, IMPORT_FEATURE).await;
    }
}

/// Rule: the same decision gates the EXPORT under its own object action — a ceiling that does
/// not admit the write withholds it, and a refused export writes no object.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_ceiling_decides_the_exports_object_write() {
    let (router, _engine) = emulator();
    let identifier = "emu-pg-ceiling-export";
    create_bucket(&router, BUCKET, REGION).await;
    let (address, port) = available_instance(&router, identifier, "postgres", &[]).await;
    let mut session = master_session("the export's session", &address, port).await;
    must_run(
        "the extension",
        &mut session,
        "CREATE EXTENSION IF NOT EXISTS aws_s3 CASCADE",
    )
    .await;
    must_run(
        "the exported table",
        &mut session,
        "CREATE TABLE exported (id integer, name text)",
    )
    .await;
    must_run(
        "the exported rows",
        &mut session,
        "INSERT INTO exported (id, name) VALUES (1, 'alpha')",
    )
    .await;

    for (index, row) in EXPORT_ROWS.iter().enumerate() {
        let name = format!("emu-ceiling-export-{index}");
        let key = format!("export/ceiling-{index}.csv");
        let role_arn =
            role_under_a_ceiling(&router, row.case, &name, row.policy, row.ceiling).await;
        associate_active(&router, row.case, identifier, &role_arn, EXPORT_FEATURE).await;
        let export = format!(
            "SELECT * FROM aws_s3.query_export_to_s3('SELECT id, name FROM exported', \
             '{BUCKET}', '{key}', '{REGION}', options := '(format csv)')"
        );
        let answered = session.query(&export).await;
        let stored = s3(&router).get_object().bucket(BUCKET).key(&key).send().await;
        if row.served {
            answered.unwrap_or_else(|refusal| {
                panic!(
                    "case {:?}: the export must be served, got {refusal:?}",
                    row.case
                )
            });
            let bytes = stored
                .unwrap_or_else(|error| {
                    panic!(
                        "case {:?}: Amazon S3 holds the exported object: {error}",
                        row.case
                    )
                })
                .body
                .collect()
                .await
                .expect("the exported object's bytes")
                .to_vec();
            assert_eq!(
                String::from_utf8_lossy(&bytes),
                "1,alpha\n",
                "case {:?}: expected the query's row as the object's bytes, got {:?}",
                row.case,
                String::from_utf8_lossy(&bytes)
            );
        } else {
            let refusal = match answered {
                Ok(rows) => panic!(
                    "case {:?}: the export must be refused, got {rows:?}",
                    row.case
                ),
                Err(refusal) => refusal,
            };
            let withheld = format!(
                "credentials for the s3export feature could not be accessed: S3 API returned \
                 error: AccessDenied: Access Denied (s3:PutObject on \
                 arn:aws:s3:::{BUCKET}/{key})"
            );
            assert_eq!(
                refusal.message, withheld,
                "case {:?}: expected Amazon S3's access-denied error, got {refusal:?}",
                row.case
            );
            assert_eq!(
                refusal.code, INSUFFICIENT_PRIVILEGE,
                "case {:?}: expected the insufficient-privilege SQLSTATE, got {refusal:?}",
                row.case
            );
            assert!(
                stored.is_err(),
                "case {:?}: a refused export writes no object, got {stored:?}",
                row.case
            );
        }
        disassociate(&router, row.case, identifier, &role_arn, EXPORT_FEATURE).await;
    }
}
