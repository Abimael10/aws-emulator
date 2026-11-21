// SPDX-License-Identifier: CC0-1.0

//! What a read of a LAKE FORMATION-governed AWS Glue Data Catalog table answers, over every
//! FORM of statement that reads one — driven end to end over the wire through the same router
//! the emulator boots.
//!
//! THE RULE UNDER TEST: Athena resolves every relation a DML statement reads through the data
//! catalog the statement addresses (`<catalog>.<database>.<table>`, however the statement
//! writes, qualifies or quotes that reference), reads the objects under the resolved table's
//! `StorageDescriptor.Location`, and narrows them to the cells the caller's Lake Formation
//! `SELECT` grant covers. The resolution and the narrowing therefore belong to the RELATION
//! and not to the shape of the statement around it: a star, an explicit projection, an
//! aggregate, an added predicate, a join, a derived table, a common table expression, a
//! grouping and a self-join over one governed relation each answer exactly the granted columns
//! and the rows the grant's own predicate keeps. A relation no `SELECT` of the caller's reaches
//! and a relation the catalog holds nothing for are ONE answer: the execution settles `FAILED`
//! carrying Athena's own not-found class for the relation — the user error category, the
//! entity-not-found error type, and the `TABLE_NOT_FOUND` sentence naming the catalog, the
//! database and the table with the line and column the statement wrote that reference at —
//! never the wording of the engine the statement ran on, which names no catalog relation at
//! all.
//!
//! Lake Formation owns those grants and Glue owns the tables they name, so Lake Formation,
//! Glue and Amazon S3 are mounted beside Athena on ONE shared engine (the way the production
//! router mounts them) and every database, table, object, data cells filter and grant below is
//! written through those services' own wire APIs: what the read path consults is the state
//! those calls wrote. The reader is an IAM user other than the principal the catalog records as
//! the creator of these tables, which is what makes a narrowing observable at all.
//!
//! WHY an ungranted relation is asserted to be indistinguishable from an absent one, rather
//! than refused with an authorization error: that is the answer this service's governance
//! module states and implements (`crate`'s `state::lake_formation` and the `Hidden` arm of
//! `state::catalog_sql`), it is the answer the behavioural checks this suite was written
//! alongside accept for a table whose grant was never made or has been revoked, and it is what
//! keeps a reader from learning that a relation it may not read exists. Should a recording ever
//! show the authorization wording instead, this expectation and that hiding rule change
//! together — neither on its own.
//!
//! The rows of each table differ along what the rule generalises over: the FORM of the
//! statement that reads the relation, how the reference is qualified, quoted and cased, WHERE
//! in the statement it is written, and which narrowing the grant carries (every column with a
//! row predicate, a named subset of the columns with no predicate, nothing at all). Nothing
//! here asserts a value this emulator invented: the rows are the bytes written under each
//! table's location, and a failure's sentence is built from the statement that was submitted.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The name Athena reaches the account's AWS Glue Data Catalog by, which is also the catalog a
/// `TABLE_NOT_FOUND` sentence names.
const GLUE_CATALOG: &str = "awsdatacatalog";

/// The bucket every table keeps its data in, and where the queries write their results.
const BUCKET: &str = "athena-governed-forms";
const OUTPUT_LOCATION: &str = "s3://athena-governed-forms/results/";

/// The database holding the tables: its create hands the tables created in it NO
/// `IAM_ALLOWED_PRINCIPALS` default, which is the state a client that has handed its catalog to
/// Lake Formation is in, so Lake Formation's own grants are what decides every read below.
const DATABASE: &str = "governed_forms";

/// The access key id the reader signs with, and the IAM user it belongs to.
const READER_KEY_ID: &str = "AKIAEXAMPLEFORMSREADER";
const READER_NAME: &str = "forms-reader";

/// The table read through a grant covering every column and keeping only the rows its
/// predicate matches, the table read through a grant covering two of its three columns, and
/// the table nothing grants the reader anything on.
const FILTERED_TABLE: &str = "sales";
const COLUMN_SCOPED_TABLE: &str = "payroll";
const UNGRANTED_TABLE: &str = "ledger";

/// A relation the catalog holds nothing for at all, which must fail exactly as an ungranted
/// one does.
const ABSENT_TABLE: &str = "absent_ledger";

/// The second governed table a join reads, granted whole.
const JOINED_TABLE: &str = "regions";

/// The columns the three like-shaped tables declare, in order, with the Hive types the catalog
/// records, and the object written under each one's location.
const FACT_COLUMNS: [(&str, &str); 3] = [("id", "bigint"), ("name", "string"), ("zone", "string")];
const FACT_DATA: &str = "1,alpha,west\n2,beta,east\n3,gamma,west\n4,delta,east\n";

/// The columns and the object of the table a join reads.
const JOINED_COLUMNS: [(&str, &str); 2] = [("zone", "string"), ("label", "string")];
const JOINED_DATA: &str = "west,Western\neast,Eastern\n";

/// The predicate the data cells filter over [`FILTERED_TABLE`] carries
/// (`RowFilter$FilterExpression`), which keeps two of the four rows written under it.
const ROW_PREDICATE: &str = "zone = 'west'";

/// The two columns of [`COLUMN_SCOPED_TABLE`] the reader is granted
/// (`TableWithColumnsResource$ColumnNames`).
const GRANTED_COLUMNS: [&str; 2] = ["id", "name"];

/// The `AthenaError$ErrorCategory` of a failure the caller's own request is the cause of: the
/// documented category list is "1 - System, 2 - User, 3 - Other".
const ERROR_CATEGORY_USER: i64 = 2;

/// The `AthenaError$ErrorType` Athena publishes for the NOT-FOUND class: a statement naming a
/// table or a database the data catalog does not hold.
const ERROR_TYPE_ENTITY_NOT_FOUND: i64 = 1301;

/// How long a poll waits for a started query to settle. Generous: what is asserted is the
/// state the execution reaches, never how fast the host reached it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The ARN the account holds an IAM user under, which is how a Lake Formation grant names it.
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

/// Athena, Glue, Amazon S3 and Lake Formation mounted on ONE shared engine, the way the router
/// mounts every service.
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
    service: (&str, &str),
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let (name, target) = service;
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target}.{operation}"))
        .header("authorization", authorization(name, access_key_id))
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

/// One Glue call by the account's own credential, which is the principal the catalog records as
/// the creator of what it writes.
async fn glue_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) {
    let (status, answer) = call_as(router, ("glue", "AWSGlue"), "AKIDEXAMPLE", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Glue {operation} must answer 200, got {status} with {answer}"
    );
}

/// One Lake Formation call (`restJson1`: `POST /<Operation>`) that must succeed.
async fn lake_formation_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) {
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
    assert_eq!(
        status, 200,
        "case {case:?}: Lake Formation {operation} must answer 200, got {status} with {:?}",
        String::from_utf8_lossy(response.body())
    );
}

/// Write one object into [`BUCKET`], or create the bucket itself with an empty path under it.
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

/// Record the IAM user the reader's access key id belongs to, as `iam:CreateUser` and
/// `iam:CreateAccessKey` record one, with an inline policy allowing this service's actions and
/// the Data Catalog reads a query performs with the caller's identity — the reader has to be
/// allowed to START a query and to READ the catalog's metadata before Lake Formation decides
/// which cells of a table it may see, which is the pairing AWS's own `AmazonAthenaFullAccess`
/// grants (`athena:*` beside `glue:Get*`, and the object write a query saves its results with).
fn given_reader(engine: &Arc<engine::InMemoryEngine>) {
    let minted = ::aws_smithy_types::DateTime::from_secs(0);
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store.users.insert(
        iam::name_key(READER_NAME),
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::User {
            path: "/".to_string(),
            user_name: READER_NAME.to_string(),
            user_id: "AIDAEXAMPLEFORMSREADER".to_string(),
            arn: user_arn(READER_NAME),
            create_date: minted,
            password_last_used: None,
            permissions_boundary: None,
            tags: None,
        },
    );
    store.user_inline_policies.insert(
        iam::name_key(READER_NAME),
        BTreeMap::from([(
            iam::name_key("inline"),
            iam::InlinePolicy {
                policy_name: "inline".to_string(),
                policy_document: json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        { "Effect": "Allow", "Action": "athena:*", "Resource": "*" },
                        { "Effect": "Allow", "Action": "glue:Get*", "Resource": "*" },
                        { "Effect": "Allow", "Action": "s3:PutObject", "Resource": "*" },
                    ],
                })
                .to_string(),
            },
        )]),
    );
    store.access_keys.insert(
        READER_KEY_ID.to_string(),
        iam::AccessKeyRecord {
            user_key: iam::name_key(READER_NAME),
            status: iam::ACCESS_KEY_ACTIVE_STATUS.to_string(),
            create_date: minted,
        },
    );
}

/// Register the table `table` over its own prefix of [`BUCKET`], and write its data object
/// under it.
async fn create_table(
    router: &MultiServiceRouter,
    table: &str,
    columns: &[(&str, &str)],
    data: &str,
) {
    let case = "setup";
    s3_put(router, case, &format!("/{BUCKET}/{DATABASE}/{table}/data.csv"), data).await;
    glue_ok(
        router,
        case,
        "CreateTable",
        &json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": table,
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/{DATABASE}/{table}/"),
                    "Columns": columns
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

/// Grant `permissions` on `resource` to the reader.
async fn grant_to_reader(router: &MultiServiceRouter, resource: Value, permissions: &[&str]) {
    lake_formation_ok(
        router,
        "setup",
        "GrantPermissions",
        &json!({
            "Principal": { "DataLakePrincipalIdentifier": user_arn(READER_NAME) },
            "Resource": resource,
            "Permissions": permissions,
        }),
    )
    .await;
}

/// The whole lake every suite here reads: the bucket, the governed database, the four tables
/// and the Lake Formation state that decides each one — a data cells filter carrying a row
/// predicate over one, a column-scoped grant on another, a whole-table grant on the third, and
/// nothing at all on the fourth.
async fn governed_lake(engine: &Arc<engine::InMemoryEngine>, router: &MultiServiceRouter) {
    given_reader(engine);
    s3_put(router, "setup", &format!("/{BUCKET}"), "").await;
    glue_ok(
        router,
        "setup",
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE, "CreateTableDefaultPermissions": [] } }),
    )
    .await;
    for table in [FILTERED_TABLE, COLUMN_SCOPED_TABLE, UNGRANTED_TABLE] {
        create_table(router, table, &FACT_COLUMNS, FACT_DATA).await;
    }
    create_table(router, JOINED_TABLE, &JOINED_COLUMNS, JOINED_DATA).await;

    // Every column of the filtered table, and only the rows its predicate keeps.
    lake_formation_ok(
        router,
        "setup",
        "CreateDataCellsFilter",
        &json!({
            "TableData": {
                "TableCatalogId": ::engine::emulator_account_id(),
                "DatabaseName": DATABASE,
                "TableName": FILTERED_TABLE,
                "Name": "visible_rows",
                "ColumnWildcard": {},
                "RowFilter": { "FilterExpression": ROW_PREDICATE },
            },
        }),
    )
    .await;
    grant_to_reader(
        router,
        json!({
            "DataCellsFilter": {
                "TableCatalogId": ::engine::emulator_account_id(),
                "DatabaseName": DATABASE,
                "TableName": FILTERED_TABLE,
                "Name": "visible_rows",
            }
        }),
        &["SELECT"],
    )
    .await;
    // Two of the three columns of the column-scoped table, with no predicate at all.
    grant_to_reader(
        router,
        json!({
            "TableWithColumns": {
                "DatabaseName": DATABASE,
                "Name": COLUMN_SCOPED_TABLE,
                "ColumnNames": GRANTED_COLUMNS,
            }
        }),
        &["SELECT"],
    )
    .await;
    // The joined table whole, so a join reads two governed relations and not one.
    grant_to_reader(
        router,
        json!({ "Table": { "DatabaseName": DATABASE, "Name": JOINED_TABLE } }),
        &["SELECT"],
    )
    .await;
}

/// Start `sql` as the reader, in the governed database, and answer the `QueryExecutionId` Athena
/// minted for it.
async fn start_as_reader(router: &MultiServiceRouter, case: &str, sql: &str) -> String {
    let (status, answer) = call_as(
        router,
        ("athena", "AmazonAthena"),
        READER_KEY_ID,
        "StartQueryExecution",
        &json!({
            "QueryString": sql,
            "QueryExecutionContext": { "Database": DATABASE, "Catalog": GLUE_CATALOG },
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

/// The `Status` of `id` once the execution has reached a TERMINAL state, polled until it does
/// or [`SETTLE_DEADLINE`] passes — the completion is owed a span after the submit, so what is
/// waited for is the condition and never a fixed time.
async fn settled_status(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let started = Instant::now();
    loop {
        let (status, body) = call_as(
            router,
            ("athena", "AmazonAthena"),
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

/// The column names and the data rows `id`'s result set carries, with the label row the first
/// page of a DML result set opens with dropped.
async fn results(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
) -> (Vec<String>, Vec<Vec<String>>) {
    let (status, body) = call_as(
        router,
        ("athena", "AmazonAthena"),
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

/// One row of the statement-form table: a statement over a governed relation, and the columns
/// and rows the reader must be served from it.
struct Form {
    /// What this row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// The names of the result columns, in projection order.
    columns: &'static [&'static str],
    /// The data rows, in the order the statement asked for them.
    rows: &'static [&'static [&'static str]],
}

/// The rows: every form of statement that reads a governed relation sees the same narrowing.
fn read_forms() -> Vec<Form> {
    vec![
        Form {
            case: "a star over the quoted, database-qualified reference",
            sql: "SELECT * FROM \"governed_forms\".\"sales\" ORDER BY id",
            columns: &["id", "name", "zone"],
            rows: &[&["1", "alpha", "west"], &["3", "gamma", "west"]],
        },
        Form {
            case: "an explicit projection of an unqualified reference",
            sql: "SELECT name, zone FROM sales ORDER BY name",
            columns: &["name", "zone"],
            rows: &[&["alpha", "west"], &["gamma", "west"]],
        },
        Form {
            case: "the catalog, the database and the table all written",
            sql: "SELECT * FROM awsdatacatalog.governed_forms.sales ORDER BY id",
            columns: &["id", "name", "zone"],
            rows: &[&["1", "alpha", "west"], &["3", "gamma", "west"]],
        },
        Form {
            case: "the database and the table written in mixed case",
            sql: "SELECT * FROM Governed_Forms.Sales ORDER BY id",
            columns: &["id", "name", "zone"],
            rows: &[&["1", "alpha", "west"], &["3", "gamma", "west"]],
        },
        Form {
            case: "an aggregate over the relation, which counts the granted rows only",
            sql: "SELECT count(*) AS visible_rows FROM sales",
            columns: &["visible_rows"],
            rows: &[&["2"]],
        },
        Form {
            case: "a predicate of the statement's own, inside the grant's",
            sql: "SELECT id FROM sales WHERE name = 'gamma'",
            columns: &["id"],
            rows: &[&["3"]],
        },
        Form {
            case: "a predicate of the statement's own, outside the grant's",
            sql: "SELECT id FROM sales WHERE zone = 'east'",
            columns: &["id"],
            rows: &[],
        },
        Form {
            case: "a join of two governed relations, each read through its own grant",
            sql: "SELECT s.name, r.label FROM sales s JOIN regions r ON r.zone = s.zone \
                  ORDER BY s.name",
            columns: &["name", "label"],
            rows: &[&["alpha", "Western"], &["gamma", "Western"]],
        },
        Form {
            case: "a derived table reading the relation",
            sql: "SELECT n FROM (SELECT name AS n FROM sales) sub ORDER BY n",
            columns: &["n"],
            rows: &[&["alpha"], &["gamma"]],
        },
        Form {
            case: "a common table expression over the relation",
            sql: "WITH visible AS (SELECT zone FROM sales) SELECT count(*) AS n FROM visible",
            columns: &["n"],
            rows: &[&["2"]],
        },
        Form {
            case: "a grouping over the relation",
            sql: "SELECT zone, count(*) AS n FROM sales GROUP BY zone",
            columns: &["zone", "n"],
            rows: &[&["west", "2"]],
        },
        Form {
            case: "the same relation read twice in one statement",
            sql: "SELECT count(*) AS n FROM sales a JOIN sales b ON a.id = b.id",
            columns: &["n"],
            rows: &[&["2"]],
        },
        Form {
            case: "a star over a relation whose grant names two of its three columns",
            sql: "SELECT * FROM payroll ORDER BY id",
            columns: &["id", "name"],
            rows: &[
                &["1", "alpha"],
                &["2", "beta"],
                &["3", "gamma"],
                &["4", "delta"],
            ],
        },
        Form {
            case: "an aggregate over that column-scoped relation",
            sql: "SELECT count(*) AS n FROM payroll",
            columns: &["n"],
            rows: &[&["4"]],
        },
        Form {
            case: "an aggregate over one granted column of it",
            sql: "SELECT max(name) AS latest FROM payroll",
            columns: &["latest"],
            rows: &[&["gamma"]],
        },
    ]
}

/// Every form of statement that reads a governed relation is answered from the catalog's own
/// resolution of that relation and the grant the reader holds on it: the granted columns, and
/// the rows the grant's predicate keeps.
#[tokio::test]
async fn every_statement_form_reads_the_cells_the_grant_covers() {
    let (engine, router) = emulator();
    governed_lake(&engine, &router).await;

    let forms = read_forms();
    // Every statement is submitted before any is waited on: a completion is owed a span after
    // its submit, and the rows are independent of one another.
    let mut started: Vec<String> = Vec::new();
    for form in &forms {
        started.push(start_as_reader(&router, form.case, form.sql).await);
    }

    for (form, id) in forms.iter().zip(&started) {
        let case = form.case;
        let status = settled_status(&router, case, id).await;
        let state = status["State"].as_str().unwrap_or_default();
        assert_eq!(
            state, QUERY_STATE_SUCCEEDED,
            "case {case:?}: the granted read must succeed, got state {state:?} with {status}"
        );
        let (columns, rows) = results(&router, case, id).await;
        assert_eq!(
            columns, form.columns,
            "case {case:?}: expected the granted columns {:?}, got {columns:?}",
            form.columns
        );
        let expected: Vec<Vec<String>> = form
            .rows
            .iter()
            .map(|values| values.iter().map(|value| (*value).to_string()).collect())
            .collect();
        assert_eq!(
            rows, expected,
            "case {case:?}: expected the granted rows {expected:?}, got {rows:?}"
        );
    }
}

/// One row of the unresolvable-relation table: a statement whose relation the reader cannot
/// resolve, and the reference it wrote for it.
struct Refused {
    /// What this row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// The reference the statement wrote for that relation, as it wrote it — what fixes the
    /// line and column the failure names.
    written: &'static str,
    /// The table the catalog folds that reference's table name to, as the failure names it.
    table: &'static str,
}

/// The rows: a relation no grant of the reader's reaches and a relation the catalog holds
/// nothing for, written in each of the forms and positions a statement can write one.
fn refused_forms() -> Vec<Refused> {
    vec![
        Refused {
            case: "an ungranted relation, quoted and database-qualified",
            sql: "SELECT * FROM \"governed_forms\".\"ledger\"",
            written: "\"governed_forms\".\"ledger\"",
            table: UNGRANTED_TABLE,
        },
        Refused {
            case: "an ungranted relation, unqualified",
            sql: "SELECT * FROM ledger",
            written: "ledger",
            table: UNGRANTED_TABLE,
        },
        Refused {
            case: "an ungranted relation, written in mixed case",
            sql: "SELECT * FROM Ledger",
            written: "Ledger",
            table: UNGRANTED_TABLE,
        },
        Refused {
            case: "an ungranted relation inside a derived table",
            sql: "SELECT count(*) AS n FROM (SELECT * FROM ledger) sub",
            written: "ledger",
            table: UNGRANTED_TABLE,
        },
        Refused {
            case: "an ungranted relation joined to a granted one",
            sql: "SELECT s.name FROM sales s JOIN ledger l ON l.id = s.id",
            written: "ledger",
            table: UNGRANTED_TABLE,
        },
        Refused {
            case: "an ungranted relation inside a common table expression",
            sql: "WITH hidden AS (SELECT * FROM ledger) SELECT count(*) AS n FROM hidden",
            written: "ledger",
            table: UNGRANTED_TABLE,
        },
        Refused {
            case: "an ungranted relation written on a later line",
            sql: "SELECT *\n  FROM ledger\n ORDER BY id",
            written: "ledger",
            table: UNGRANTED_TABLE,
        },
        Refused {
            case: "a relation the catalog holds nothing for at all",
            sql: "SELECT * FROM absent_ledger",
            written: "absent_ledger",
            table: ABSENT_TABLE,
        },
    ]
}

impl Refused {
    /// The sentence Athena words for this row's failure: the error code, the position the
    /// statement wrote the reference at — the 1-based line and column of its first character —
    /// and the relation fully qualified by the catalog and the database the request addressed.
    fn expected_message(&self) -> String {
        let at = self
            .sql
            .find(self.written)
            .unwrap_or_else(|| panic!("case {:?}: {:?} is written in the statement", self.case, self.written));
        let before = &self.sql[..at];
        let line = 1 + before.matches('\n').count();
        let column = 1 + before.len() - before.rfind('\n').map_or(0, |newline| newline + 1);
        format!(
            "TABLE_NOT_FOUND: line {line}:{column}: Table \
             '{GLUE_CATALOG}.{DATABASE}.{}' does not exist",
            self.table
        )
    }
}

/// A relation the reader holds no grant on and a relation the catalog holds nothing for are the
/// same answer, in every form and position a statement writes one: Athena's own not-found class
/// for the relation the statement named — and never the wording of the engine the statement
/// would have run on, which knows no catalog relation and would name the reference as the
/// statement spelled it.
#[tokio::test]
async fn an_unresolvable_relation_fails_in_athenas_own_not_found_class() {
    let (engine, router) = emulator();
    governed_lake(&engine, &router).await;

    let forms = refused_forms();
    let mut started: Vec<String> = Vec::new();
    for form in &forms {
        started.push(start_as_reader(&router, form.case, form.sql).await);
    }

    for (form, id) in forms.iter().zip(&started) {
        let case = form.case;
        let status = settled_status(&router, case, id).await;
        let state = status["State"].as_str().unwrap_or_default();
        assert_eq!(
            state, QUERY_STATE_FAILED,
            "case {case:?}: a relation the reader cannot resolve must not be readable, got \
             state {state:?} with {status}"
        );
        let expected = form.expected_message();
        assert_eq!(
            status["StateChangeReason"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the not-found reason {expected:?}, got {status}"
        );
        let error = &status["AthenaError"];
        assert_eq!(
            error["ErrorMessage"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the same sentence as the error message, got {status}"
        );
        assert_eq!(
            error["ErrorCategory"].as_i64(),
            Some(ERROR_CATEGORY_USER),
            "case {case:?}: expected the user error category {ERROR_CATEGORY_USER}, got {status}"
        );
        assert_eq!(
            error["ErrorType"].as_i64(),
            Some(ERROR_TYPE_ENTITY_NOT_FOUND),
            "case {case:?}: expected the entity-not-found error type \
             {ERROR_TYPE_ENTITY_NOT_FOUND}, got {status}"
        );
    }
}
