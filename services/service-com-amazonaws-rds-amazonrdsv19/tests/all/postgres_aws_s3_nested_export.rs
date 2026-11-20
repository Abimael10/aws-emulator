// SPDX-License-Identifier: CC0-1.0

//! What `aws_s3.query_export_to_s3` exports: a query OF THE DATABASE, never a call of the
//! extension itself.
//!
//! RULE: the query the export runs is run as a statement of the calling session, down the
//! same statement path that recognises and dispatches this extension's own functions — so a
//! query that names the extension's schema would re-enter the export once per level, with the
//! nesting chosen by the statement rather than by this endpoint. "The SQL query that the
//! PostgreSQL server runs" is a query of the database, so one naming that schema is refused
//! before it runs, in PostgreSQL's own words for a construct the server does not serve where
//! it was written (`0A000`), and nothing is exported for it; every other query the endpoint
//! serves exports exactly as before.
//!
//! The suite drives a stock PostgreSQL client against the instance's own engine endpoint, with
//! Amazon S3 and IAM mounted beside this service on ONE engine, through the harness the sibling
//! `aws_s3` suite builds.

use crate::db_instance_engine_endpoint::PostgresSession;
use crate::db_instances::REGION;
use crate::postgres_aws_s3_import_export::{
    associate, available_postgres, refusal, run, session_with_the_extension, BUCKET,
};
use crate::rds_s3_integration::{create_bucket, emulator, role, s3};

/// The service principal an S3-integrated instance's role trusts.
const RDS_PRINCIPAL: &str = "rds.amazonaws.com";

/// A policy granting the object write an export performs, on every object of the bucket.
fn put_object_policy() -> String {
    serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["s3:PutObject", "s3:AbortMultipartUpload"],
            "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
        }],
    })
    .to_string()
}

/// One query an export is asked to run, and whether the endpoint serves it.
struct QueryRow {
    case: &'static str,
    /// The query argument of the export call, as the statement spells it.
    query: &'static str,
    /// Whether the endpoint exports it at all.
    served: bool,
}

/// The queries this rule spans: a plain read, a read whose text merely carries the schema's
/// name without naming it, and the places a nested call can sit — the `FROM` list, the select
/// list, and a second level below either. Each is passed as the call's query argument under
/// dollar quoting, the form a client writes a query carrying its own quotes with, so the rows
/// differ in the QUERY and in nothing else.
const QUERY_ROWS: &[QueryRow] = &[
    QueryRow {
        case: "a query of the database",
        query: "SELECT id, name FROM exported ORDER BY id",
        served: true,
    },
    QueryRow {
        case: "a query whose own text carries the schema's name without naming the schema",
        query: "SELECT id AS aws_s3_id, name FROM exported ORDER BY id",
        served: true,
    },
    QueryRow {
        case: "a query that is itself an export, in the FROM list",
        query: "SELECT * FROM aws_s3.query_export_to_s3('SELECT id FROM exported', \
                'emu-pg-aws-s3', 'nested/one.csv', 'us-west-2')",
        served: false,
    },
    QueryRow {
        case: "a query that is itself an import, in the select list",
        query: "SELECT aws_s3.table_import_from_s3('exported', '', '(format csv)', \
                'emu-pg-aws-s3', 'nested/two.csv', 'us-west-2')",
        served: false,
    },
    QueryRow {
        case: "a query that nests another call below itself",
        query: "SELECT * FROM (SELECT aws_s3.table_import_from_s3('exported', '', \
                '(format csv)', 'emu-pg-aws-s3', 'nested/three.csv', 'us-west-2')) AS nested",
        served: false,
    },
];

/// The export runs a query of the database and refuses one that calls the extension itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_export_refuses_a_query_that_calls_the_extension_itself() {
    let (router, _engine) = emulator();
    let identifier = "emu-pg-export-nested";
    create_bucket(&router, BUCKET, REGION).await;
    let writer = role(
        &router,
        "emu-pg-export-nested-role",
        RDS_PRINCIPAL,
        Some(put_object_policy()),
    )
    .await;
    let (address, port) = available_postgres(&router, identifier).await;
    associate(&router, identifier, &writer, "s3Export").await;
    let mut session: PostgresSession = session_with_the_extension(&address, port).await;
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

    for (index, row) in QUERY_ROWS.iter().enumerate() {
        let key = format!("nested/attempt-{index}.csv");
        let statement = format!(
            "SELECT * FROM aws_s3.query_export_to_s3($query${}$query$, '{BUCKET}', '{key}', \
             '{REGION}', options := '(format csv)')",
            row.query
        );
        if row.served {
            run(&mut session, row.case, &statement).await;
            let stored = s3(&router)
                .get_object()
                .bucket(BUCKET)
                .key(&key)
                .send()
                .await;
            assert!(
                stored.is_ok(),
                "case {:?}: the export must have written {key}, got {:?}",
                row.case,
                stored.err()
            );
            continue;
        }
        let answered = refusal(&mut session, row.case, &statement).await;
        assert_eq!(
            answered.code, "0A000",
            "case {:?}: expected the feature-not-supported state, got {answered:?}",
            row.case
        );
        assert_eq!(
            answered.message,
            "the query aws_s3.query_export_to_s3 exports must not itself call a function of \
             the aws_s3 schema",
            "case {:?}: expected the export's own sentence for it, got {answered:?}",
            row.case
        );
        // A refused export writes nothing: the destination holds no object for it.
        let stored = s3(&router)
            .get_object()
            .bucket(BUCKET)
            .key(&key)
            .send()
            .await;
        assert!(
            stored.is_err(),
            "case {:?}: a refused export writes no object, got one at {key}",
            row.case
        );
    }
}
