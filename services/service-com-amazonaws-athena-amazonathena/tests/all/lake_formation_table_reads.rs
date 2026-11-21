// SPDX-License-Identifier: CC0-1.0

//! What a query may read of a Data Catalog table that LAKE FORMATION governs, driven end
//! to end over the wire through the same router the emulator boots.
//!
//! THE RULE UNDER TEST: a table of the AWS Glue Data Catalog is readable through a query
//! only while Lake Formation resolves a `SELECT` permission on it for the calling
//! principal — in its own name, through the `IAM_ALLOWED_PRINCIPALS` group every IAM user
//! and role belongs to, or as an administrator of the data lake — and the result set then
//! carries only the COLUMNS and the ROWS that grant covers. A table no such permission
//! reaches is not there for that caller: the execution settles FAILED reporting the
//! catalog's own not-found class for the relation, exactly as a table the catalog does not
//! hold does, so a reader cannot tell a hidden table from an absent one.
//!
//! Lake Formation OWNS those grants, so it is mounted beside Athena, Glue and Amazon S3 on
//! ONE shared engine (the set the production router builds) and every grant, revoke, filter
//! and settings write here goes through Lake Formation's own wire API: what the query path
//! consults is the state those calls wrote. The reader is an IAM USER other than the one
//! the Data Catalog records as the creator of these tables — what a table's creator holds on
//! it is a grant of its own — which is what makes a refusal observable at all.
//!
//! The first suite is a TABLE whose rows differ along what the rule generalises over: which
//! resource the grant names (the table, its named columns, a column wildcard with an
//! exclusion, every table of the database, a data cells filter over it, another table),
//! which permission it carries, which principal holds it, and whether the database still
//! hands its tables the `IAM_ALLOWED_PRINCIPALS` default — and each row asserts the columns
//! AND the rows the reader is served.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use smithy_server_runtime::MultiServiceRouter;

/// The name Athena reaches the account's AWS Glue Data Catalog by, which is also the
/// catalog a `TABLE_NOT_FOUND` names.
const GLUE_CATALOG: &str = "awsdatacatalog";

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The bucket every table keeps its data in.
const BUCKET: &str = "athena-lake-formation-data";

/// Where the queries write their results.
const OUTPUT_LOCATION: &str = "s3://athena-lake-formation-data/results/";

/// The database whose tables Lake Formation governs: its create hands the tables created in
/// it NO `IAM_ALLOWED_PRINCIPALS` default, which is the state a client that has handed its
/// catalog to Lake Formation is in.
const GOVERNED: &str = "governed_lake";

/// A database whose create named no table defaults, so its tables carry the account's "use
/// only IAM access control" grant.
const OPEN: &str = "open_lake";

/// A second governed database, for the row whose grant covers EVERY table of a database:
/// such a grant is about the database, so the row that holds it needs a database of its own
/// for the other rows' tables to stay outside it.
const GOVERNED_WILDCARD: &str = "wildcard_lake";

/// The Lake Formation group every IAM user and role belongs to, spelled as Lake Formation
/// spells it on the wire (`DataLakeSettings$CreateDatabaseDefaultPermissions`).
const IAM_ALLOWED_PRINCIPALS: &str = "IAM_ALLOWED_PRINCIPALS";

/// The access key id the reader signs with, and the IAM user it belongs to.
const READER_KEY_ID: &str = "AKIAEXAMPLELAKEREADER";
const READER_NAME: &str = "lake-reader";

/// A principal nothing here signs as, so a row can hold a grant for someone else.
const ANOTHER_NAME: &str = "someone-else";

/// The columns every table declares, in order, and the rows its data object holds.
const COLUMNS: [(&str, &str); 3] = [("id", "int"), ("name", "string"), ("zone", "string")];
const DATA: &str = "1,alpha,west\n2,beta,east\n3,gamma,west\n4,delta,east\n";

/// Every row of the data, as a reader granted the whole table sees it.
const EVERY_ROW: &[&[&str]] = &[
    &["1", "alpha", "west"],
    &["2", "beta", "east"],
    &["3", "gamma", "west"],
    &["4", "delta", "east"],
];

/// How long a poll waits for a started query to settle. Generous: what is asserted is that
/// the execution reaches a terminal state, never how fast the host got there.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The ARN the account holds an IAM user under, which is how a Lake Formation grant names
/// it.
fn user_arn(name: &str) -> String {
    format!("arn:aws:iam::{}:user/{name}", ::engine::emulator_account_id())
}

/// A SigV4 `Authorization` header naming `service` in its credential scope — the routing key
/// the router claims a request by — signed with `access_key_id`, which is what names the
/// principal the request speaks for. Nothing is verified: this harness turns request
/// authentication off.
fn authorization(service: &str, access_key_id: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
}

/// Athena, Glue, Amazon S3 and Lake Formation mounted on ONE shared engine, the way the
/// router mounts every service.
fn emulator() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_athena_amazonathena::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_lakeformation_awslakeformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    (engine, router)
}

/// One awsJson1_1 call to `service`, signed with `access_key_id`.
async fn call_as(
    router: &MultiServiceRouter,
    service: &str,
    target: &str,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target}.{operation}"))
        .header("authorization", authorization(service, access_key_id))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Glue call by the account's own credential, which is the principal the catalog records
/// as the creator of what it writes.
async fn glue_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) =
        call_as(router, "glue", "AWSGlue", "AKIDEXAMPLE", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Glue {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// One Lake Formation call (`restJson1`: `POST /<Operation>`) that must succeed.
async fn lake_formation_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://lakeformation.{REGION}.amazonaws.com/{operation}"
        ))
        .header("host", format!("lakeformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lakeformation", "AKIDEXAMPLE"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: Lake Formation {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// Write one object into [`BUCKET`], creating the bucket on the first call.
async fn s3_put(router: &MultiServiceRouter, case: &str, path: &str, body: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3", "AKIDEXAMPLE"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "case {case:?}: writing s3://{path} must succeed, got {}",
        response.status()
    );
}

/// Record the IAM user an access key id belongs to, as `iam:CreateUser` and
/// `iam:CreateAccessKey` record one, with an inline policy allowing this service's actions
/// and the Data Catalog reads a query performs with the caller's identity — the reader has
/// to be allowed to START a query and to READ the catalog's metadata before Lake Formation
/// decides which cells of a table it may see, which is the pairing AWS's own
/// `AmazonAthenaFullAccess` grants (`athena:*` beside `glue:Get*`).
fn given_reader(engine: &Arc<engine::InMemoryEngine>) {
    let minted = ::aws_smithy_types::DateTime::from_secs(0);
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    for (name, key) in [(READER_NAME, Some(READER_KEY_ID)), (ANOTHER_NAME, None)] {
        store.users.insert(
            iam::name_key(name),
            ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::User {
                path: "/".to_string(),
                user_name: name.to_string(),
                user_id: format!("AIDAEXAMPLE{}", name.to_ascii_uppercase().replace('-', "")),
                arn: user_arn(name),
                create_date: minted,
                password_last_used: None,
                permissions_boundary: None,
                tags: None,
            },
        );
        store.user_inline_policies.insert(
            iam::name_key(name),
            BTreeMap::from([(
                iam::name_key("inline"),
                iam::InlinePolicy {
                    policy_name: "inline".to_string(),
                    policy_document: json!({
                        "Version": "2012-10-17",
                        "Statement": [
                            { "Effect": "Allow", "Action": "athena:*", "Resource": "*" },
                            { "Effect": "Allow", "Action": "glue:Get*", "Resource": "*" },
                            // Athena saves a query's results with the CALLER's identity, so
                            // a principal that runs queries holds the object write too —
                            // the permission AWS's own Athena policy grants beside these.
                            { "Effect": "Allow", "Action": "s3:PutObject", "Resource": "*" },
                        ],
                    })
                    .to_string(),
                },
            )]),
        );
        if let Some(key) = key {
            store.access_keys.insert(
                key.to_string(),
                iam::AccessKeyRecord {
                    user_key: iam::name_key(name),
                    status: iam::ACCESS_KEY_ACTIVE_STATUS.to_string(),
                    create_date: minted,
                },
            );
        }
    }
}

/// Create the two databases every suite uses and the bucket their tables keep their data
/// in: the governed database, whose create hands its tables no default grant, and the open
/// one, whose create names no defaults at all.
async fn create_databases(router: &MultiServiceRouter, case: &str) {
    s3_put(router, case, &format!("/{BUCKET}"), "").await;
    for governed in [GOVERNED, GOVERNED_WILDCARD] {
        glue_ok(
            router,
            case,
            "CreateDatabase",
            &json!({
                "DatabaseInput": { "Name": governed, "CreateTableDefaultPermissions": [] },
            }),
        )
        .await;
    }
    glue_ok(
        router,
        case,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": OPEN } }),
    )
    .await;
}

/// Register the table `table` of `database` over its own prefix, and write the data object
/// under it.
async fn create_table(router: &MultiServiceRouter, case: &str, database: &str, table: &str) {
    s3_put(
        router,
        case,
        &format!("/{BUCKET}/{database}/{table}/data.csv"),
        DATA,
    )
    .await;
    glue_ok(
        router,
        case,
        "CreateTable",
        &json!({
            "DatabaseName": database,
            "TableInput": {
                "Name": table,
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/{database}/{table}/"),
                    "Columns": COLUMNS
                        .iter()
                        .map(|(name, data_type)| json!({ "Name": name, "Type": data_type }))
                        .collect::<Vec<Value>>(),
                    // The data objects are comma-separated text, which is the SerDe the
                    // table's own definition has to say it is read by.
                    "SerdeInfo": {
                        "SerializationLibrary":
                            "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe",
                        "Parameters": { "field.delim": "," },
                    },
                },
            },
        }),
    )
    .await;
}

/// Take away every grant the table `table` of `database` was created carrying for the group
/// every IAM principal belongs to, so Lake Formation's own grants are what decides a read of
/// it.
///
/// Only a database whose create named no `CreateTableDefaultPermissions` hands its tables
/// that grant, so only such a table has one to take away.
async fn lock_down(router: &MultiServiceRouter, case: &str, database: &str, table: &str) {
    if database != OPEN {
        return;
    }
    lake_formation_ok(
        router,
        case,
        "RevokePermissions",
        &json!({
            "Principal": { "DataLakePrincipalIdentifier": IAM_ALLOWED_PRINCIPALS },
            "Resource": { "Table": { "DatabaseName": database, "Name": table } },
            "Permissions": ["ALL"],
        }),
    )
    .await;
}

/// Grant `permissions` on `resource` to `principal`.
async fn grant(
    router: &MultiServiceRouter,
    case: &str,
    principal: &str,
    resource: Value,
    permissions: &[&str],
) {
    lake_formation_ok(
        router,
        case,
        "GrantPermissions",
        &json!({
            "Principal": { "DataLakePrincipalIdentifier": principal },
            "Resource": resource,
            "Permissions": permissions,
        }),
    )
    .await;
}

/// Start `sql` as the reader and answer the `QueryExecutionId` it minted.
async fn start_as_reader(
    router: &MultiServiceRouter,
    case: &str,
    database: &str,
    sql: &str,
) -> String {
    let (status, answer) = call_as(
        router,
        "athena",
        "AmazonAthena",
        READER_KEY_ID,
        "StartQueryExecution",
        &json!({
            "QueryString": sql,
            "QueryExecutionContext": { "Database": database, "Catalog": GLUE_CATALOG },
            "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: StartQueryExecution({sql}) must be accepted, got {status} with {answer}"
    );
    answer["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a query execution id, got {answer}"))
        .to_string()
}

/// The `Status` of `id` once the execution has reached a TERMINAL state, polled until it
/// does or [`SETTLE_DEADLINE`] passes — the completion is owed a span after the submit, so
/// what is waited for is the condition and never a fixed time.
async fn settled_status(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let started = Instant::now();
    loop {
        let (status, body) = call_as(
            router,
            "athena",
            "AmazonAthena",
            READER_KEY_ID,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetQueryExecution must answer 200, got {status} with {body}"
        );
        let reported = body["QueryExecution"]["Status"].clone();
        let state = reported["State"].as_str().unwrap_or_default().to_string();
        if state == QUERY_STATE_SUCCEEDED || state == QUERY_STATE_FAILED {
            return reported;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the execution never reached a terminal state, last saw {reported}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The column names and the data rows `id`'s result set carries, with the label row the
/// first page of a DML result set opens with dropped.
async fn results(router: &MultiServiceRouter, case: &str, id: &str) -> (Vec<String>, Vec<Vec<String>>) {
    let (status, body) = call_as(
        router,
        "athena",
        "AmazonAthena",
        READER_KEY_ID,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id, "MaxResults": 100 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueryResults must answer 200, got {status} with {body}"
    );
    let result_set = &body["ResultSet"];
    let columns: Vec<String> = result_set["ResultSetMetadata"]["ColumnInfo"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|column| column["Name"].as_str().unwrap_or_default().to_string())
        .collect();
    let rows: Vec<Vec<String>> = result_set["Rows"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .skip(1)
        .map(|row| {
            row["Data"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|datum| datum["VarCharValue"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .collect();
    (columns, rows)
}

/// What Lake Formation holds for the reader on one table.
enum Held {
    /// Nothing is revoked: the table still carries the default its database hands its
    /// tables.
    CreationDefault,
    /// That default is revoked and nothing is granted.
    Nothing,
    /// The default is revoked and `permissions` are granted to the reader on the resource
    /// `resource` builds for that table.
    Granted {
        resource: fn(&str, &str) -> Value,
        permissions: &'static [&'static str],
    },
    /// The default is revoked and `permissions` are granted to ANOTHER principal on the
    /// table.
    GrantedToAnother {
        permissions: &'static [&'static str],
    },
    /// The default is revoked, a data cells filter over the table is created, and `SELECT`
    /// on THAT filter is granted to the reader.
    CellFilter {
        /// The columns the filter names, or `None` for its column wildcard.
        columns: Option<&'static [&'static str]>,
        /// The row predicate it carries.
        row_filter: &'static str,
    },
}

/// `Resource` naming the table itself.
fn on_table(database: &str, table: &str) -> Value {
    json!({ "Table": { "DatabaseName": database, "Name": table } })
}

/// `Resource` naming every table of the database.
fn on_every_table(database: &str, _table: &str) -> Value {
    json!({ "Table": { "DatabaseName": database, "TableWildcard": {} } })
}

/// `Resource` naming two of the table's three columns.
fn on_two_columns(database: &str, table: &str) -> Value {
    json!({
        "TableWithColumns": {
            "DatabaseName": database,
            "Name": table,
            "ColumnNames": ["id", "name"],
        }
    })
}

/// `Resource` naming every column of the table but one.
fn on_all_but_one_column(database: &str, table: &str) -> Value {
    json!({
        "TableWithColumns": {
            "DatabaseName": database,
            "Name": table,
            "ColumnWildcard": { "ExcludedColumnNames": ["zone"] },
        }
    })
}

/// `Resource` naming ANOTHER table of the same database.
fn on_another_table(database: &str, _table: &str) -> Value {
    json!({ "Table": { "DatabaseName": database, "Name": "t_elsewhere" } })
}

/// One row of the governance table: the Lake Formation state of one table, and what the
/// reader must then be served from it.
struct Case {
    /// What this row is about.
    case: &'static str,
    /// The database the table lives in.
    database: &'static str,
    /// The table, which is this row's own.
    table: &'static str,
    /// What Lake Formation holds for the reader on it.
    held: Held,
    /// The columns the reader must see, in order, or `None` when the table must be hidden
    /// from it.
    columns: Option<&'static [&'static str]>,
    /// The rows the reader must see, projected to those columns.
    rows: &'static [&'static [&'static str]],
}

/// The rows of the governance table.
fn cases() -> Vec<Case> {
    vec![
        Case {
            case: "the IAM_ALLOWED_PRINCIPALS default its database hands its tables",
            database: OPEN,
            table: "t_open",
            held: Held::CreationDefault,
            columns: Some(&["id", "name", "zone"]),
            rows: EVERY_ROW,
        },
        Case {
            case: "nothing at all, once that default is revoked",
            database: OPEN,
            table: "t_revoked",
            held: Held::Nothing,
            columns: None,
            rows: &[],
        },
        Case {
            case: "nothing at all, in a database that hands its tables no default",
            database: GOVERNED,
            table: "t_ungranted",
            held: Held::Nothing,
            columns: None,
            rows: &[],
        },
        Case {
            case: "SELECT on the table itself",
            database: GOVERNED,
            table: "t_select",
            held: Held::Granted {
                resource: on_table,
                permissions: &["SELECT"],
            },
            columns: Some(&["id", "name", "zone"]),
            rows: EVERY_ROW,
        },
        Case {
            case: "ALL on the table, the permission on the resource as a whole",
            database: GOVERNED,
            table: "t_all",
            held: Held::Granted {
                resource: on_table,
                permissions: &["ALL"],
            },
            columns: Some(&["id", "name", "zone"]),
            rows: EVERY_ROW,
        },
        Case {
            case: "SELECT on every table of the database",
            database: GOVERNED_WILDCARD,
            table: "t_wildcard",
            held: Held::Granted {
                resource: on_every_table,
                permissions: &["SELECT"],
            },
            columns: Some(&["id", "name", "zone"]),
            rows: EVERY_ROW,
        },
        Case {
            case: "DESCRIBE on the table, which is not the permission a read needs",
            database: GOVERNED,
            table: "t_describe",
            held: Held::Granted {
                resource: on_table,
                permissions: &["DESCRIBE"],
            },
            columns: None,
            rows: &[],
        },
        Case {
            case: "SELECT on two of the table's three columns",
            database: GOVERNED,
            table: "t_columns",
            held: Held::Granted {
                resource: on_two_columns,
                permissions: &["SELECT"],
            },
            columns: Some(&["id", "name"]),
            rows: &[
                &["1", "alpha"],
                &["2", "beta"],
                &["3", "gamma"],
                &["4", "delta"],
            ],
        },
        Case {
            case: "SELECT on a column wildcard that excludes one column",
            database: GOVERNED,
            table: "t_excluded",
            held: Held::Granted {
                resource: on_all_but_one_column,
                permissions: &["SELECT"],
            },
            columns: Some(&["id", "name"]),
            rows: &[
                &["1", "alpha"],
                &["2", "beta"],
                &["3", "gamma"],
                &["4", "delta"],
            ],
        },
        Case {
            case: "SELECT through a data cells filter carrying a row predicate",
            database: GOVERNED,
            table: "t_rows",
            held: Held::CellFilter {
                columns: None,
                row_filter: "\"zone\" = 'west'",
            },
            columns: Some(&["id", "name", "zone"]),
            rows: &[&["1", "alpha", "west"], &["3", "gamma", "west"]],
        },
        Case {
            case: "SELECT through a data cells filter narrowing both columns and rows",
            database: GOVERNED,
            table: "t_cells",
            held: Held::CellFilter {
                columns: Some(&["id", "zone"]),
                row_filter: "\"zone\" = 'east'",
            },
            columns: Some(&["id", "zone"]),
            rows: &[&["2", "east"], &["4", "east"]],
        },
        Case {
            case: "SELECT on ANOTHER table of the same database",
            database: GOVERNED,
            table: "t_other_granted",
            held: Held::Granted {
                resource: on_another_table,
                permissions: &["SELECT"],
            },
            columns: None,
            rows: &[],
        },
        Case {
            case: "SELECT on the table, held by ANOTHER principal",
            database: GOVERNED,
            table: "t_someone_else",
            held: Held::GrantedToAnother {
                permissions: &["SELECT"],
            },
            columns: None,
            rows: &[],
        },
    ]
}

/// Put one row's Lake Formation state in place, through Lake Formation's own API.
async fn apply(router: &MultiServiceRouter, row: &Case) {
    match &row.held {
        Held::CreationDefault => {}
        Held::Nothing => lock_down(router, row.case, row.database, row.table).await,
        Held::Granted {
            resource,
            permissions,
        } => {
            lock_down(router, row.case, row.database, row.table).await;
            grant(
                router,
                row.case,
                &user_arn(READER_NAME),
                resource(row.database, row.table),
                permissions,
            )
            .await;
        }
        Held::GrantedToAnother { permissions } => {
            lock_down(router, row.case, row.database, row.table).await;
            grant(
                router,
                row.case,
                &user_arn(ANOTHER_NAME),
                on_table(row.database, row.table),
                permissions,
            )
            .await;
        }
        Held::CellFilter {
            columns,
            row_filter,
        } => {
            lock_down(router, row.case, row.database, row.table).await;
            let mut filter = json!({
                "TableCatalogId": ::engine::emulator_account_id(),
                "DatabaseName": row.database,
                "TableName": row.table,
                "Name": "cells",
                "RowFilter": { "FilterExpression": row_filter },
            });
            match columns {
                Some(named) => filter["ColumnNames"] = json!(named),
                None => filter["ColumnWildcard"] = json!({}),
            }
            lake_formation_ok(
                router,
                row.case,
                "CreateDataCellsFilter",
                &json!({ "TableData": filter }),
            )
            .await;
            grant(
                router,
                row.case,
                &user_arn(READER_NAME),
                json!({
                    "DataCellsFilter": {
                        "TableCatalogId": ::engine::emulator_account_id(),
                        "DatabaseName": row.database,
                        "TableName": row.table,
                        "Name": "cells",
                    }
                }),
                &["SELECT"],
            )
            .await;
        }
    }
}

/// What a hidden relation's failure says: the catalog's own not-found class for the
/// relation the statement named, which carries the catalog, database and table of the
/// request and the position of the name in the submitted SQL.
fn table_not_found(sql: &str, database: &str, table: &str) -> String {
    let column = sql
        .find(&format!("\"{database}\""))
        .map(|position| position + 1)
        .unwrap_or_default();
    format!(
        "TABLE_NOT_FOUND: line 1:{column}: Table \
         '{GLUE_CATALOG}.{database}.{table}' does not exist"
    )
}

#[tokio::test]
async fn a_query_reads_exactly_the_cells_lake_formation_grants() {
    let (engine, router) = emulator();
    given_reader(&engine);
    create_databases(&router, "setup").await;
    // The table a row may grant on INSTEAD of its own, so that grant names a table that
    // exists and the row still shows the read refused.
    create_table(&router, "setup", GOVERNED, "t_elsewhere").await;

    let cases = cases();
    let mut started: Vec<(usize, String, String)> = Vec::new();
    for (index, row) in cases.iter().enumerate() {
        create_table(&router, row.case, row.database, row.table).await;
        apply(&router, row).await;
        let sql = format!("SELECT * FROM \"{}\".\"{}\"", row.database, row.table);
        let id = start_as_reader(&router, row.case, row.database, &sql).await;
        started.push((index, sql, id));
    }

    for (index, sql, id) in &started {
        let row = &cases[*index];
        let case = row.case;
        let status = settled_status(&router, case, id).await;
        let state = status["State"].as_str().unwrap_or_default();
        match row.columns {
            None => {
                assert_eq!(
                    state, QUERY_STATE_FAILED,
                    "case {case:?}: a table no SELECT of the reader's reaches must not be \
                     readable, got state {state:?} with {status}"
                );
                let expected = table_not_found(sql, row.database, row.table);
                assert_eq!(
                    status["StateChangeReason"].as_str(),
                    Some(expected.as_str()),
                    "case {case:?}: expected the not-found reason {expected:?}, got {status}"
                );
                assert_eq!(
                    status["AthenaError"]["ErrorMessage"].as_str(),
                    Some(expected.as_str()),
                    "case {case:?}: expected the same sentence as the error message, got \
                     {status}"
                );
            }
            Some(expected_columns) => {
                assert_eq!(
                    state, QUERY_STATE_SUCCEEDED,
                    "case {case:?}: the granted read must succeed, got state {state:?} with \
                     {status}"
                );
                let (columns, rows) = results(&router, case, id).await;
                assert_eq!(
                    columns, expected_columns,
                    "case {case:?}: expected the granted columns {expected_columns:?}, got \
                     {columns:?}"
                );
                let expected_rows: Vec<Vec<String>> = row
                    .rows
                    .iter()
                    .map(|values| values.iter().map(|value| (*value).to_string()).collect())
                    .collect();
                assert_eq!(
                    rows, expected_rows,
                    "case {case:?}: expected the granted rows {expected_rows:?}, got {rows:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_column_outside_the_grant_is_not_addressable() {
    // The other half of a column grant: the excluded column is not a column of the table as
    // the reader sees it, so naming it fails as an unknown column rather than returning its
    // values — while the granted columns still read.
    let case = "the excluded column";
    let (engine, router) = emulator();
    given_reader(&engine);
    create_databases(&router, case).await;
    create_table(&router, case, GOVERNED, "t_columns").await;
    lock_down(&router, case, GOVERNED, "t_columns").await;
    grant(
        &router,
        case,
        &user_arn(READER_NAME),
        on_two_columns(GOVERNED, "t_columns"),
        &["SELECT"],
    )
    .await;

    let granted = start_as_reader(
        &router,
        case,
        GOVERNED,
        &format!("SELECT \"id\", \"name\" FROM \"{GOVERNED}\".\"t_columns\""),
    )
    .await;
    let excluded = start_as_reader(
        &router,
        case,
        GOVERNED,
        &format!("SELECT \"zone\" FROM \"{GOVERNED}\".\"t_columns\""),
    )
    .await;

    let status = settled_status(&router, case, &granted).await;
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: the granted columns must read, got {status}"
    );
    let status = settled_status(&router, case, &excluded).await;
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_FAILED),
        "case {case:?}: the excluded column must not be addressable, got {status}"
    );
    // Athena's own class for a name that cannot be resolved, in the wording it uses where
    // the reader's own permissions are why: fixed text either side of the column the
    // statement wrote, and no position — the reader is not told which of the two it is.
    let expected = "COLUMN_NOT_FOUND: Column 'zone' cannot be resolved or requester is not \
                    authorized to access requested resources";
    assert_eq!(
        status["StateChangeReason"].as_str(),
        Some(expected),
        "case {case:?}: expected the unresolvable-column reason {expected:?}, got {status}"
    );
    assert_eq!(
        status["AthenaError"]["ErrorMessage"].as_str(),
        Some(expected),
        "case {case:?}: expected the same sentence as the error message, got {status}"
    );

    // A column the table DOES NOT DECLARE at all is the same class in Athena's other
    // wording: the position the statement wrote the name at, and no clause about
    // permissions — nothing is being hidden from this reader there.
    let absent = start_as_reader(
        &router,
        case,
        GOVERNED,
        &format!("SELECT \"nowhere\" FROM \"{GOVERNED}\".\"t_columns\""),
    )
    .await;
    let status = settled_status(&router, case, &absent).await;
    let expected = "COLUMN_NOT_FOUND: line 1:8: Column 'nowhere' cannot be resolved";
    assert_eq!(
        status["StateChangeReason"].as_str(),
        Some(expected),
        "case {case:?}: expected the unresolvable-column reason {expected:?}, got {status}"
    );
}

#[tokio::test]
async fn a_filter_naming_a_column_the_table_lacks_fails_in_the_engines_own_words() {
    // The other side of that class: a name the SUBMITTED statement never wrote is not the
    // caller's to be told about in Athena's unresolvable-column sentence. A row filter whose
    // own predicate names a column the table does not declare fails the read, and the reason
    // is the engine's own about the name IT could not resolve — never a claim that the
    // statement named it, and never the authorization wording.
    let case = "a filter over a column the table lacks";
    let (engine, router) = emulator();
    given_reader(&engine);
    create_databases(&router, case).await;
    create_table(&router, case, GOVERNED, "t_badfilter").await;
    lake_formation_ok(
        &router,
        case,
        "CreateDataCellsFilter",
        &json!({
            "TableData": {
                "TableCatalogId": ::engine::emulator_account_id(),
                "DatabaseName": GOVERNED,
                "TableName": "t_badfilter",
                "Name": "cells",
                "RowFilter": { "FilterExpression": "\"region\" = 'west'" },
                "ColumnWildcard": {},
            },
        }),
    )
    .await;
    grant(
        &router,
        case,
        &user_arn(READER_NAME),
        json!({
            "DataCellsFilter": {
                "TableCatalogId": ::engine::emulator_account_id(),
                "DatabaseName": GOVERNED,
                "TableName": "t_badfilter",
                "Name": "cells",
            }
        }),
        &["SELECT"],
    )
    .await;

    let id = start_as_reader(
        &router,
        case,
        GOVERNED,
        &format!("SELECT * FROM \"{GOVERNED}\".\"t_badfilter\""),
    )
    .await;
    let status = settled_status(&router, case, &id).await;
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_FAILED),
        "case {case:?}: a predicate the engine cannot resolve must fail the read, got {status}"
    );
    let reason = status["StateChangeReason"].as_str().unwrap_or_default();
    assert!(
        !reason.starts_with("COLUMN_NOT_FOUND"),
        "case {case:?}: the statement named no such column, so the failure must not be \
         reported as one of its names, got {reason:?}"
    );
    assert!(
        reason.contains("region"),
        "case {case:?}: the failure must name what could not be resolved, got {reason:?}"
    );
}

#[tokio::test]
async fn revoking_the_grant_hides_the_table_again() {
    // The grant is not consulted once and remembered: the same statement reads the table
    // while the grant is there and stops reading it as soon as the grant is revoked, with
    // the table reported exactly as one the catalog does not hold.
    let case = "a revoked SELECT";
    let (engine, router) = emulator();
    given_reader(&engine);
    create_databases(&router, case).await;
    create_table(&router, case, GOVERNED, "t_rev").await;
    lock_down(&router, case, GOVERNED, "t_rev").await;
    grant(
        &router,
        case,
        &user_arn(READER_NAME),
        on_table(GOVERNED, "t_rev"),
        &["SELECT"],
    )
    .await;

    let sql = format!("SELECT * FROM \"{GOVERNED}\".\"t_rev\"");
    let granted = start_as_reader(&router, case, GOVERNED, &sql).await;
    let status = settled_status(&router, case, &granted).await;
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: the granted read must succeed, got {status}"
    );
    let (_, rows) = results(&router, case, &granted).await;
    assert_eq!(
        rows.len(),
        EVERY_ROW.len(),
        "case {case:?}: the granted read must return every row, got {rows:?}"
    );

    lake_formation_ok(
        &router,
        case,
        "RevokePermissions",
        &json!({
            "Principal": { "DataLakePrincipalIdentifier": user_arn(READER_NAME) },
            "Resource": on_table(GOVERNED, "t_rev"),
            "Permissions": ["SELECT"],
        }),
    )
    .await;

    let after = start_as_reader(&router, case, GOVERNED, &sql).await;
    let status = settled_status(&router, case, &after).await;
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_FAILED),
        "case {case:?}: after the revoke the read must stop working, got {status}"
    );
    let expected = table_not_found(&sql, GOVERNED, "t_rev");
    assert_eq!(
        status["StateChangeReason"].as_str(),
        Some(expected.as_str()),
        "case {case:?}: expected the not-found reason {expected:?}, got {status}"
    );
}

#[tokio::test]
async fn a_row_filter_is_an_expression_and_not_a_second_statement() {
    // The predicate a grant carries is a boolean EXPRESSION the engine evaluates, never a
    // statement it runs: text that closes the predicate and writes another statement after
    // it fails the read instead of being performed, and the table is still whole afterwards.
    let case = "a predicate carrying a second statement";
    let (engine, router) = emulator();
    given_reader(&engine);
    create_databases(&router, case).await;
    create_table(&router, case, GOVERNED, "t_guard").await;
    lake_formation_ok(
        &router,
        case,
        "CreateDataCellsFilter",
        &json!({
            "TableData": {
                "TableCatalogId": ::engine::emulator_account_id(),
                "DatabaseName": GOVERNED,
                "TableName": "t_guard",
                "Name": "cells",
                "RowFilter": {
                    "FilterExpression":
                        format!("1=1); DROP TABLE \"{GOVERNED}.t_guard\"; SELECT (1"),
                },
                "ColumnWildcard": {},
            },
        }),
    )
    .await;
    grant(
        &router,
        case,
        &user_arn(READER_NAME),
        json!({
            "DataCellsFilter": {
                "TableCatalogId": ::engine::emulator_account_id(),
                "DatabaseName": GOVERNED,
                "TableName": "t_guard",
                "Name": "cells",
            }
        }),
        &["SELECT"],
    )
    .await;

    let sql = format!("SELECT * FROM \"{GOVERNED}\".\"t_guard\"");
    let id = start_as_reader(&router, case, GOVERNED, &sql).await;
    let status = settled_status(&router, case, &id).await;
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_FAILED),
        "case {case:?}: a predicate that is not an expression must fail the read, got {status}"
    );

    // And the table is still there, whole, for a reader the grant lets read it all.
    grant(
        &router,
        case,
        &user_arn(READER_NAME),
        on_table(GOVERNED, "t_guard"),
        &["SELECT"],
    )
    .await;
    let id = start_as_reader(&router, case, GOVERNED, &sql).await;
    let status = settled_status(&router, case, &id).await;
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: the table must still read, got {status}"
    );
    let (columns, rows) = results(&router, case, &id).await;
    assert_eq!(
        columns.len(),
        COLUMNS.len(),
        "case {case:?}: every column is still there, got {columns:?}"
    );
    assert_eq!(
        rows.len(),
        EVERY_ROW.len(),
        "case {case:?}: every row is still there, got {rows:?}"
    );
}

#[tokio::test]
async fn an_administrator_of_the_data_lake_reads_a_table_nothing_granted_it() {
    // A data lake administrator holds every permission of its catalog implicitly, so it
    // reads the whole table with no grant of its own — the control that says a refusal above
    // is about what the reader HOLDS and not about the table being unreadable.
    let case = "the data lake administrator";
    let (engine, router) = emulator();
    given_reader(&engine);
    create_databases(&router, case).await;
    create_table(&router, case, GOVERNED, "t_admin").await;
    lock_down(&router, case, GOVERNED, "t_admin").await;
    lake_formation_ok(
        &router,
        case,
        "PutDataLakeSettings",
        &json!({
            "DataLakeSettings": {
                "DataLakeAdmins": [
                    { "DataLakePrincipalIdentifier": user_arn(READER_NAME) },
                ],
            },
        }),
    )
    .await;

    let id = start_as_reader(
        &router,
        case,
        GOVERNED,
        &format!("SELECT * FROM \"{GOVERNED}\".\"t_admin\""),
    )
    .await;
    let status = settled_status(&router, case, &id).await;
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: an administrator's read must succeed, got {status}"
    );
    let (columns, rows) = results(&router, case, &id).await;
    assert_eq!(
        columns,
        vec!["id".to_string(), "name".to_string(), "zone".to_string()],
        "case {case:?}: an administrator sees every column, got {columns:?}"
    );
    assert_eq!(
        rows.len(),
        EVERY_ROW.len(),
        "case {case:?}: an administrator sees every row, got {rows:?}"
    );
}
