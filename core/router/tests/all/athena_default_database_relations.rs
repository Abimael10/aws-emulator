// SPDX-License-Identifier: CC0-1.0

//! The database the DATA CATALOG itself holds, and how a statement naming a relation
//! nobody registered is therefore classified — driven across the services the question
//! spans, mounted on ONE engine exactly as `core/router/src/lib.rs` mounts them.
//!
//! RULE: the Data Catalog `AwsDataCatalog` names holds its own `default` database for
//! every account, without anybody creating one — it is the database a statement that
//! carries no `QueryExecutionContext` resolves unqualified names in — so a statement
//! naming a relation that catalog does not hold fails on the RELATION, with Athena's
//! table-not-found class naming `catalog.database.table` and the position the statement
//! wrote it at, while only a database the catalog really does not hold is reported as the
//! schema that is not there. The failure a query ends on is what every reader of that
//! query carries: `GetQueryExecution`'s `Status.AthenaError`, and the `States.TaskFailed`
//! cause of a Step Functions `Task` state that ran the query through the optimized Athena
//! integration under the run-a-job (`.sync`) pattern, whose cause IS that JSON
//! `QueryExecution` document.
//!
//! Three crates have to agree for that to hold (Athena runs the statement and classifies
//! the failure, Glue holds the catalog the names are resolved in, and Step Functions
//! reports what Athena answered), so the suite lives here rather than in any of them.
//!
//! The rows differ along what the rule generalises over: whether the statement qualifies
//! the relation at all, whether its context names the database, whether the database is
//! the catalog's own or one a client created, whether the database is one the catalog
//! does not hold, and where in the statement the relation was written.
//!
//! Nothing here asserts a value this emulator invented: every expectation is built from
//! the statement that was submitted, the `default` database is read back through Glue's
//! own `GetDatabase`, and the task's cause is compared with what Athena's own
//! `GetQueryExecution` reports for the query the task ran.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The role, the state machine and the execution reader the Step Functions suites here
// already build: `tests/all` is one crate root, so this suite borrows them rather than
// carrying a second copy.
use crate::sfn_task_target_authorization::{
    allow, logs_only, machine, ran, role, string, task_failure, ACCOUNT, REGION,
};

/// How long a poll waits for a query, or an execution, to reach a terminal state.
///
/// Generous rather than tight: a query's completion is due a span after its submission and
/// a parked task is resolved a poll interval after that, so this deadline is only the
/// point at which the work is declared never to have happened — a loaded host must not be
/// the thing that trips it.
const PATIENCE: Duration = Duration::from_secs(120);

/// How often a poll asks again.
const POLL: Duration = Duration::from_millis(25);

/// The name Athena addresses the account's Glue Data Catalog by, folded the way its query
/// layer names it: "AwsDataCatalog" is the catalog, and every identifier the query layer
/// reports is lowercased.
const GLUE_CATALOG: &str = "awsdatacatalog";

/// The database the Data Catalog holds for every account, which is also the one a
/// statement with no `QueryExecutionContext` resolves unqualified names in.
const CATALOG_DEFAULT_DATABASE: &str = "default";

/// The relation no row of this suite registers anywhere.
const ABSENT_RELATION: &str = "no_such_relation_here";

/// A database a client creates through Glue, so one row resolves in a database the catalog
/// holds because somebody wrote it.
const CREATED_DATABASE: &str = "catalog_written_db";

/// A database nothing ever creates.
const ABSENT_DATABASE: &str = "never_created_db";

/// The `AthenaError$ErrorCategory` of a failure the caller's own input is the cause of:
/// the documented category list is "1 - System, 2 - User, 3 - Other".
const ERROR_CATEGORY_USER: i64 = 2;

/// The `AthenaError$ErrorType` Athena publishes for the NOT-FOUND class.
const ERROR_TYPE_ENTITY_NOT_FOUND: i64 = 1301;

/// Where a query writes its results. Nothing here reads them: a location is what lets a
/// query run at all ("if none of them is set, Athena issues an error that no output
/// location is provided"), and every statement below fails before producing any.
const RESULTS_LOCATION: &str = "s3://athena-default-database-results/results/";

/// The `Resource` the Step Functions row names: the Athena query integration under the
/// run-a-job pattern.
const START_QUERY_SYNC: &str = "arn:aws:states:::athena:startQueryExecution.sync";

/// The IAM actions the pattern's two calls are taken under.
const START_ACTION: &str = "athena:StartQueryExecution";
const REPORT_ACTION: &str = "athena:GetQueryExecution";

/// The Data Catalog reads the statement itself performs, which Athena performs with the
/// identity the query was submitted under — so the role a task runs the query as has to
/// hold them before the statement reaches the relation it names at all. The pairing is the
/// one AWS's own `AmazonAthenaFullAccess` grants (`athena:*` beside `glue:Get*`).
const CATALOG_READ_ACTION: &str = "glue:Get*";

/// The workgroup a query runs in when the request names none.
const DEFAULT_WORK_GROUP: &str = "primary";

/// The ASL error name a task whose own work failed reports.
const TASK_FAILED_ERROR: &str = "States.TaskFailed";

/// The states a query is still working in.
const PENDING_STATES: [&str; 2] = ["QUEUED", "RUNNING"];

/// The services this question spans, over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_athena_amazonathena::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is the
/// routing key the router claims a request by. Nothing is verified: this router runs with
/// request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
}

/// Call one operation of an `awsJson1_1` service, and answer its status and body.
async fn json_try(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The same call, requiring a 200.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = json_try(router, signing_name, target_prefix, operation, body).await;
    assert_eq!(
        status, 200,
        "case {operation}: expected 200, got {status} with {parsed:?}"
    );
    parsed
}

/// One Athena call (`awsJson1_1` under the target prefix `AmazonAthena`).
async fn athena(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    json_call(router, "athena", "AmazonAthena", operation, body).await
}

/// One Glue call (`awsJson1_1` under the target prefix `AWSGlue`).
async fn glue(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    json_call(router, "glue", "AWSGlue", operation, body).await
}

/// One Glue call that is expected to be refused, as its status and body.
async fn glue_refusal(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    json_try(router, "glue", "AWSGlue", operation, body).await
}

/// Submit `sql` and answer the `QueryExecutionId` Athena minted for it, with `database` as
/// the statement's `QueryExecutionContext` where a row names one.
async fn submit(
    router: &MultiServiceRouter,
    sql: &str,
    database: Option<&str>,
    case: &str,
) -> String {
    let mut request = json!({
        "QueryString": sql,
        "ResultConfiguration": { "OutputLocation": RESULTS_LOCATION },
    });
    if let Some(database) = database {
        request["QueryExecutionContext"] = json!({ "Database": database });
    }
    let started = athena(router, "StartQueryExecution", &request).await;
    started
        .get("QueryExecutionId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueryExecutionId in {started:?}"))
        .to_string()
}

/// Athena's own report of the query `query_id`, once that query is terminal.
async fn settled(router: &MultiServiceRouter, query_id: &str, case: &str) -> Value {
    let began = Instant::now();
    loop {
        let reported = athena(
            router,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": query_id }),
        )
        .await;
        let state = reported["QueryExecution"]["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if !PENDING_STATES.contains(&state.as_str()) {
            return reported;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected a terminal query within {PATIENCE:?}, got {reported:?}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// Athena's message for a statement naming a relation the database it resolves in does not
/// hold: the fixed text of the table-not-found class, the position the statement wrote the
/// relation at, and the relation qualified by the catalog and database it resolved in.
fn table_not_found(column: usize, database: &str, table: &str) -> String {
    format!(
        "TABLE_NOT_FOUND: line 1:{column}: Table '{GLUE_CATALOG}.{database}.{table}' does not \
         exist"
    )
}

/// Athena's message for a statement naming a database the catalog does not hold.
fn schema_not_found(column: usize, database: &str) -> String {
    format!("SCHEMA_NOT_FOUND: line 1:{column}: Schema '{database}' does not exist")
}

/// One row: the case, the statement, the database its context names, and the message the
/// failed query must carry.
struct Row {
    case: &'static str,
    sql: String,
    context: Option<&'static str>,
    message: String,
}

/// A statement naming a relation the Data Catalog does not hold fails on the RELATION in
/// every database the catalog holds — its own `default` one included — and on the SCHEMA
/// only for a database the catalog really does not hold.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relation_the_catalog_does_not_hold_is_reported_as_the_relation_that_is_absent() {
    let router = emulator();
    // A database written through the catalog's own API, so one row resolves in a database
    // that is there because a client created it.
    glue(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": CREATED_DATABASE } }),
    )
    .await;

    // `SELECT * FROM ` is 14 characters, so the relation after it starts at column 15;
    // `SELECT count(*) FROM ` is 21, so it starts at column 22.
    let rows = vec![
        Row {
            case: "an unqualified relation, with no context naming a database",
            sql: format!("SELECT * FROM {ABSENT_RELATION}"),
            context: None,
            message: table_not_found(15, CATALOG_DEFAULT_DATABASE, ABSENT_RELATION),
        },
        Row {
            case: "an unqualified relation whose context names the catalog's own database",
            sql: format!("SELECT * FROM {ABSENT_RELATION}"),
            context: Some(CATALOG_DEFAULT_DATABASE),
            message: table_not_found(15, CATALOG_DEFAULT_DATABASE, ABSENT_RELATION),
        },
        Row {
            case: "a relation the statement qualifies with the catalog's own database",
            sql: format!("SELECT * FROM {CATALOG_DEFAULT_DATABASE}.{ABSENT_RELATION}"),
            context: None,
            message: table_not_found(15, CATALOG_DEFAULT_DATABASE, ABSENT_RELATION),
        },
        Row {
            case: "the same relation written further into the statement",
            sql: format!("SELECT count(*) FROM {ABSENT_RELATION}"),
            context: None,
            message: table_not_found(22, CATALOG_DEFAULT_DATABASE, ABSENT_RELATION),
        },
        Row {
            case: "a relation in a database a client created",
            sql: format!("SELECT * FROM {CREATED_DATABASE}.{ABSENT_RELATION}"),
            context: None,
            message: table_not_found(15, CREATED_DATABASE, ABSENT_RELATION),
        },
        Row {
            case: "a relation in a database the catalog does not hold",
            sql: format!("SELECT * FROM {ABSENT_DATABASE}.{ABSENT_RELATION}"),
            context: None,
            message: schema_not_found(15, ABSENT_DATABASE),
        },
    ];

    // Every row is submitted first and read afterwards, so the suite waits once for the
    // completion every query owes rather than once per row.
    let mut submitted: Vec<(&Row, String)> = Vec::new();
    for row in &rows {
        let id = submit(&router, &row.sql, row.context, row.case).await;
        submitted.push((row, id));
    }

    for (row, id) in submitted {
        let case = row.case;
        let reported = settled(&router, &id, case).await;
        let status = &reported["QueryExecution"]["Status"];
        assert_eq!(
            status["State"].as_str(),
            Some("FAILED"),
            "case {case:?}: expected a FAILED query, got {reported:?}"
        );
        assert_eq!(
            status["AthenaError"]["ErrorMessage"].as_str(),
            Some(row.message.as_str()),
            "case {case:?}: expected the message {:?}, got {:?}",
            row.message,
            status["AthenaError"]["ErrorMessage"]
        );
        assert_eq!(
            status["AthenaError"]["ErrorCategory"].as_i64(),
            Some(ERROR_CATEGORY_USER),
            "case {case:?}: expected the user error category, got {status:?}"
        );
        assert_eq!(
            status["AthenaError"]["ErrorType"].as_i64(),
            Some(ERROR_TYPE_ENTITY_NOT_FOUND),
            "case {case:?}: expected the not-found error type, got {status:?}"
        );
    }
}

/// The `default` database a statement resolves in is the DATA CATALOG's own record of it:
/// Glue's own read reports it before any statement has run, its listing carries it, and
/// running statements leaves that record exactly as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_data_catalog_holds_the_default_database_athena_resolves_in() {
    let router = emulator();
    let case = "the database the catalog holds";

    // Before anything has run: the catalog already holds the database, so nothing had to
    // create it and no statement had to be submitted for a Glue client to find it.
    let read = glue(
        &router,
        "GetDatabase",
        &json!({ "Name": CATALOG_DEFAULT_DATABASE }),
    )
    .await;
    assert_eq!(
        read["Database"]["Name"].as_str(),
        Some(CATALOG_DEFAULT_DATABASE),
        "case {case:?}: expected the catalog to hold {CATALOG_DEFAULT_DATABASE:?}, got {read:?}"
    );
    assert_eq!(
        read["Database"]["CatalogId"].as_str(),
        Some(ACCOUNT),
        "case {case:?}: expected the account's own catalog to own it, got {read:?}"
    );
    let listed = glue(&router, "GetDatabases", &json!({})).await;
    let names: Vec<&str> = listed["DatabaseList"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["Name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        names,
        vec![CATALOG_DEFAULT_DATABASE],
        "case {case:?}: expected the listing to report the database it holds, got {listed:?}"
    );

    // Statements resolve in it and change nothing about it: no query wrote the record, so
    // neither its id nor its create instant moves under a later reader.
    for sql in [
        format!("SELECT * FROM {ABSENT_RELATION}"),
        format!("SELECT * FROM {CATALOG_DEFAULT_DATABASE}.{ABSENT_RELATION}"),
    ] {
        submit(&router, &sql, None, case).await;
    }
    let again = glue(
        &router,
        "GetDatabase",
        &json!({ "Name": CATALOG_DEFAULT_DATABASE }),
    )
    .await;
    assert_eq!(
        again["Database"], read["Database"],
        "case {case:?}: expected the same database record, got {again:?} after {read:?}"
    );
}

/// That database is a RECORD like any other the catalog holds: a client writes a table
/// into it, and a create of it is refused as one that already exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_database_the_catalog_comes_with_is_a_record_like_any_other() {
    let router = emulator();
    let case = "the database the catalog comes with";
    let table = "carried_events";

    // A table written into it is readable back through the catalog's own read.
    glue(
        &router,
        "CreateTable",
        &json!({
            "DatabaseName": CATALOG_DEFAULT_DATABASE,
            "TableInput": { "Name": table, "TableType": "EXTERNAL_TABLE" },
        }),
    )
    .await;
    let read = glue(
        &router,
        "GetTable",
        &json!({ "DatabaseName": CATALOG_DEFAULT_DATABASE, "Name": table }),
    )
    .await;
    assert_eq!(
        read["Table"]["DatabaseName"].as_str(),
        Some(CATALOG_DEFAULT_DATABASE),
        "case {case:?}: expected the table to belong to it, got {read:?}"
    );

    // A create of a database that is already there is the catalog's own refusal.
    let (status, refused) = glue_refusal(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": CATALOG_DEFAULT_DATABASE } }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: a create of a database already there must be refused, got {status} \
         with {refused:?}"
    );
    assert!(
        refused.to_string().contains("AlreadyExistsException"),
        "case {case:?}: expected AlreadyExistsException, got {refused:?}"
    );
}

/// A run-a-job (`.sync`) Athena task over such a statement fails with ASL's own
/// `States.TaskFailed`, and its cause is the JSON `QueryExecution` document Athena
/// answered for the query the task ran — carrying the failed state and the message of the
/// table-not-found class.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sync_athena_task_fails_with_athenas_own_query_execution_document() {
    let router = emulator();
    let case = "a sync task over a relation the catalog does not hold";
    let work_group_arn =
        format!("arn:aws:athena:{REGION}:{ACCOUNT}:workgroup/{DEFAULT_WORK_GROUP}");
    let role_arn = role(
        &router,
        "AthenaMissingRelationRole",
        &json!({
            "Version": "2012-10-17",
            "Statement": [
                logs_only(),
                allow(START_ACTION, &work_group_arn),
                allow(REPORT_ACTION, &work_group_arn),
                allow(CATALOG_READ_ACTION, "*"),
            ],
        }),
    )
    .await;
    let machine_arn = machine(
        &router,
        "athena-missing-relation",
        START_QUERY_SYNC,
        &json!({
            "QueryString": format!("SELECT * FROM {ABSENT_RELATION}"),
            "ResultConfiguration": { "OutputLocation": RESULTS_LOCATION },
        }),
        &role_arn,
    )
    .await;

    let (execution_arn, described) = ran(&router, &machine_arn, case).await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("FAILED"),
        "case {case:?}: expected the execution to end FAILED, got {described:?}"
    );
    assert_eq!(
        described.get("error").and_then(Value::as_str),
        Some(TASK_FAILED_ERROR),
        "case {case:?}: expected {TASK_FAILED_ERROR}, got {described:?}"
    );

    let details = task_failure(&router, &execution_arn, case).await;
    let cause = string(&details, "cause");
    let document: Value = serde_json::from_str(&cause)
        .unwrap_or_else(|_| panic!("case {case:?}: expected a JSON cause, got {cause:?}"));
    let query_id = document["QueryExecution"]["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the cause to name the query, got {document:?}")
        })
        .to_string();
    assert_eq!(
        document["QueryExecution"]["Status"]["State"].as_str(),
        Some("FAILED"),
        "case {case:?}: expected the cause to carry the query's failed state, got {document:?}"
    );
    assert_eq!(
        document["QueryExecution"]["Status"]["AthenaError"]["ErrorMessage"].as_str(),
        Some(table_not_found(15, CATALOG_DEFAULT_DATABASE, ABSENT_RELATION).as_str()),
        "case {case:?}: expected the table-not-found message, got {document:?}"
    );
    assert_eq!(
        document["QueryExecution"]["Status"]["AthenaError"]["ErrorCategory"].as_i64(),
        Some(ERROR_CATEGORY_USER),
        "case {case:?}: expected the user error category, got {document:?}"
    );

    // The cause is ATHENA's own document, not a sentence Step Functions wrote: the query
    // the task ran reports the same failure through Athena's own API.
    let reported = settled(&router, &query_id, case).await;
    assert_eq!(
        reported["QueryExecution"]["Status"]["AthenaError"]["ErrorMessage"],
        document["QueryExecution"]["Status"]["AthenaError"]["ErrorMessage"],
        "case {case:?}: expected the cause to be Athena's own report, got {reported:?} against \
         {document:?}"
    );
}
