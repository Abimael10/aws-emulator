// SPDX-License-Identifier: CC0-1.0

//! What registering a table's column-statistics task settings DOES on the ASSEMBLED emulator:
//! the statistics run a create with no `Schedule` starts, and what that in-flight run makes the
//! surface answer afterwards.
//!
//! The rule under test: **a create of a table's column-statistics task settings that states no
//! `Schedule` both stores the settings and starts the ONE statistics run they configure.** So
//! `GetColumnStatisticsTaskRuns` for that table reports that run carrying the `Role` and the
//! generation configuration the create stated, in a `ColumnStatisticsState` that still owes a
//! further state; while it is in flight a second create for the same table is the modelled
//! `ColumnStatisticsTaskRunningException` and persists nothing — neither a second run nor a
//! replaced role; `StopColumnStatisticsTaskRun` has that run to drive to `STOPPED`; and with
//! nothing in flight the create is served and starts a run of its own, which reads the objects
//! behind the table and settles `SUCCEEDED` with the statistics it computed readable through
//! `GetColumnStatisticsForTable`. A create whose `Role` the account's own IAM shows AWS Glue
//! cannot assume starts no run and stores no settings at all.
//!
//! Three crates have to agree for that to hold (IAM answers for the statistics role, AWS Glue
//! holds the catalog and performs the run, Amazon S3 holds the data the run reads), so the suite
//! lives here: a crate-local test can only ever see one side of it. The whole emulator is
//! assembled, exactly as the production entrypoint assembles it.
//!
//! The recorded case — one table, a create with one role whose run a create with a second role
//! collides with, then a stop and the re-create — is ONE row of the table below; the rows differ
//! along what the rule generalises over: how many columns the table declares, how many objects
//! sit behind it, whether a leading line of each object is a header, whether the create states a
//! `ColumnNameList` (and so which columns the run covers), and the `SampleSize` it states —
//! each published bound of that member included.
//!
//! No transition is waited for: the instant the clock owes the started run is moved THROUGH THE
//! GLUE STORE ([`move_the_run_due_instant`]), so the in-flight window and the settling are the
//! emulator's own rather than a race against the host.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::messages::{
    COLUMN_STATISTICS_TASK_SETTINGS_RUN_ALREADY_STARTED, ROLE_NOT_ASSUMABLE,
};
use service_com_amazonaws_glue_awsglue::state::{
    ComAmazonawsGlueAwsglueState, COLUMN_STATISTICS_TASK_RUN_STATE_STOPPED,
    COLUMN_STATISTICS_TASK_RUN_STATE_SUCCEEDED, GLUE_SERVICE_PRINCIPAL,
    TERMINAL_COLUMN_STATISTICS_TASK_RUN_STATES,
};
use smithy_server_runtime::MultiServiceRouter;

// The AWS Glue wire helper and the awsJson error reader are the Glue suites' own, so what this
// one calls and reads is exactly what those ones call and read.
use crate::cloudformation_provisions_glue_jobs::{error_code, glue, ACCOUNT};
use crate::query_wire::{element, form_encode};

/// The Region every call here is signed for: the catalog, the run and the objects it reads all
/// live in one Region.
const REGION: &str = "us-east-1";

/// The prefix, inside each row's own bucket, the table's data sits under.
const DATA_PREFIX: &str = "data/";

/// The input format of a table whose objects are lines of text, which is the layout a
/// Hive-style delimited table is stored in.
const TEXT_INPUT_FORMAT: &str = "org.apache.hadoop.mapred.TextInputFormat";

/// The serialization library of a Hive delimited-text table.
const LAZY_SIMPLE_SERDE: &str = "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe";

/// The serde property carrying a delimited table's field separator, and the table property
/// carrying how many leading lines of each object are a header rather than data — the Hive
/// property names this emulator's readers and AWS Glue's own crawler both write.
const FIELD_DELIMITER_PROPERTY: &str = "field.delim";
const SKIP_HEADER_LINE_COUNT_PROPERTY: &str = "skip.header.line.count";

/// The sentence AWS words the in-flight refusal with, spelled out here so the WORDING itself is
/// pinned rather than compared with the constant the handler answers from.
const RUN_ALREADY_STARTED_SENTENCE: &str =
    "Column statistics task for input table has already started";

/// Far enough out that no owed transition falls due while a case asserts the in-flight state.
const NOT_YET_DUE: Duration = Duration::from_secs(3_600);

/// Far enough into the past that the next read finds the owed transition due however coarse the
/// host clock is.
const ALREADY_DUE: Duration = Duration::from_secs(1);

/// One row of the registration table: the table a create registers settings for, what the create
/// configures, and which columns the run it starts therefore covers.
struct Registration {
    /// What this row varies, named in each of its assertions.
    case: &'static str,
    /// The columns the stored table declares, as `(name, declared type)`.
    columns: &'static [(&'static str, &'static str)],
    /// The objects staged under [`DATA_PREFIX`], as `(key, body)`.
    objects: &'static [(&'static str, &'static str)],
    /// How many leading lines of each object are a header rather than data.
    header_lines: usize,
    /// The `ColumnNameList` the create states, or `None` for a create that states none — "if
    /// none is supplied, all column names for the table will be used by default".
    column_name_list: Option<&'static [&'static str]>,
    /// The `SampleSize` the create states, or `None`. `SampleSizePercentage` is a double with
    /// `range 0..100` and a default of 0
    /// (`smithy_query(query="operation",operation="CreateColumnStatisticsTaskSettings")`), so
    /// both published bounds are rowed.
    sample_size: Option<f64>,
    /// The columns the started run must cover, in the order it reports them, each with the
    /// `ColumnStatisticsType` its declared type maps to (`smithy_query(query="shape",
    /// shape="ColumnStatisticsType")`: BOOLEAN, DATE, DECIMAL, DOUBLE, LONG, STRING, BINARY).
    covered: &'static [(&'static str, &'static str)],
}

/// The registrations the rule is asserted over. The recorded case is the first row; the others
/// move along the column count, the object count, the header line, the stated column list and
/// each published `SampleSize` bound.
fn registrations() -> Vec<Registration> {
    vec![
        Registration {
            case: "every column of a two-column table, one object with a header line, no stated \
                   sample size",
            columns: &[("id", "int"), ("city", "string")],
            objects: &[(
                "orders.csv",
                "id,city\n1,berlin\n2,lisbon\n3,berlin\n4,porto\n",
            )],
            header_lines: 1,
            column_name_list: None,
            sample_size: None,
            covered: &[("id", "LONG"), ("city", "STRING")],
        },
        Registration {
            case: "one column of three, two objects without a header, at the published upper \
                   sample size",
            columns: &[("id", "int"), ("city", "string"), ("amount", "double")],
            objects: &[
                ("north.csv", "1,berlin,10.5\n2,lisbon,20.25\n"),
                ("south.csv", "3,porto,30.75\n"),
            ],
            header_lines: 0,
            column_name_list: Some(&["city"]),
            sample_size: Some(100.0),
            covered: &[("city", "STRING")],
        },
        Registration {
            case: "two of three columns, listed out of the table's own order, at the published \
                   lower sample size",
            columns: &[("id", "int"), ("city", "string"), ("amount", "double")],
            objects: &[("rows.csv", "7,madrid,1.5\n8,madrid,2.5\n9,oslo,3.5\n")],
            header_lines: 0,
            column_name_list: Some(&["amount", "id"]),
            sample_size: Some(0.0),
            covered: &[("amount", "DOUBLE"), ("id", "LONG")],
        },
    ]
}

/// The emulator as it is ASSEMBLED, on one engine: the role IAM answers for, the catalog AWS
/// Glue holds and the objects the run reads all land in the same store the production entrypoint
/// serves from.
fn build_router_and_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    router.set_request_authentication(false);
    (router, engine)
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which is
/// what routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One Amazon S3 request against the Region's path-style endpoint, which is the endpoint a
/// statistics run reads the table's objects through.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream")
        .body(Bytes::from(body.as_bytes().to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// One awsQuery call to IAM, answered with its status and XML body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("authorization", authorization("iam"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// A trust policy admitting one service principal, which is the document that decides whether a
/// service can assume the role at all.
fn trust_policy(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// Create the role `name` through IAM's OWN `CreateRole`, trusting `principal`, and answer the
/// ARN IAM reports for it — so every role a create below names is a role the account really
/// holds rather than a string this test made up.
async fn create_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    principal: &str,
) -> String {
    let (status, created) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trust_policy(principal)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    element(&created, "Arn")
}

/// Create `bucket` and put every `(key, body)` under [`DATA_PREFIX`], so the run has data to
/// read.
async fn stage(router: &MultiServiceRouter, case: &str, bucket: &str, objects: &[(&str, &str)]) {
    let (status, body) = s3(router, "PUT", &format!("/{bucket}"), "").await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {status} with {body}"
    );
    for (key, content) in objects {
        let (status, body) = s3(
            router,
            "PUT",
            &format!("/{bucket}/{DATA_PREFIX}{key}"),
            content,
        )
        .await;
        assert!(
            (200..300).contains(&status),
            "case {case:?}: PutObject({key:?}) must succeed, got {status} with {body}"
        );
    }
}

/// Store `database` holding `table`: an external table over the row's bucket, describing the
/// layout its objects are written in so a run can read them.
async fn store_table(
    router: &MultiServiceRouter,
    case: &str,
    row: &Registration,
    bucket: &str,
    database: &str,
    table: &str,
) {
    let (status, created) = glue(
        router,
        REGION,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": database } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateDatabase({database:?}) must succeed, got {status} with {created}"
    );

    let mut descriptor = json!({
        "Location": format!("s3://{bucket}/{DATA_PREFIX}"),
        "InputFormat": TEXT_INPUT_FORMAT,
        "SerdeInfo": {
            "SerializationLibrary": LAZY_SIMPLE_SERDE,
            "Parameters": { FIELD_DELIMITER_PROPERTY: "," },
        },
        "Columns": row
            .columns
            .iter()
            .map(|(name, declared)| json!({ "Name": name, "Type": declared }))
            .collect::<Vec<Value>>(),
    });
    if row.header_lines > 0 {
        descriptor["Parameters"] =
            json!({ SKIP_HEADER_LINE_COUNT_PROPERTY: row.header_lines.to_string() });
    }
    let (status, created) = glue(
        router,
        REGION,
        "CreateTable",
        &json!({
            "DatabaseName": database,
            "TableInput": {
                "Name": table,
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": descriptor,
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTable({table:?}) must succeed, got {status} with {created}"
    );
}

/// `CreateColumnStatisticsTaskSettings` for one table as `role` with the row's configuration and
/// NO `Schedule`, and what it answered.
async fn register_settings(
    router: &MultiServiceRouter,
    row: &Registration,
    database: &str,
    table: &str,
    role: &str,
) -> (u16, Value) {
    let mut body = json!({ "DatabaseName": database, "TableName": table, "Role": role });
    if let Some(columns) = row.column_name_list {
        body["ColumnNameList"] = json!(columns);
    }
    if let Some(sample_size) = row.sample_size {
        body["SampleSize"] = json!(sample_size);
    }
    glue(router, REGION, "CreateColumnStatisticsTaskSettings", &body).await
}

/// The `ColumnStatisticsTaskRuns` the listing reports for one table, as it reports them (newest
/// first).
async fn runs_of(
    router: &MultiServiceRouter,
    case: &str,
    database: &str,
    table: &str,
) -> Vec<Value> {
    let (status, body) = glue(
        router,
        REGION,
        "GetColumnStatisticsTaskRuns",
        &json!({ "DatabaseName": database, "TableName": table }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetColumnStatisticsTaskRuns must answer 200, got {status} with {body}"
    );
    body.get("ColumnStatisticsTaskRuns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The one run the listing of a table reports, asserting there is exactly one.
async fn the_only_run_of(
    router: &MultiServiceRouter,
    case: &str,
    database: &str,
    table: &str,
) -> Value {
    let runs = runs_of(router, case, database, table).await;
    assert_eq!(
        runs.len(),
        1,
        "case {case:?}: expected exactly one run of the table, got {runs:?}"
    );
    runs[0].clone()
}

/// The `Role` the settings record of a table reports, or the wire error code of the refusal the
/// read answered.
async fn reported_settings_role(
    router: &MultiServiceRouter,
    database: &str,
    table: &str,
) -> Result<String, String> {
    let (status, body) = glue(
        router,
        REGION,
        "GetColumnStatisticsTaskSettings",
        &json!({ "DatabaseName": database, "TableName": table }),
    )
    .await;
    if status != 200 {
        return Err(error_code(&body));
    }
    Ok(body
        .pointer("/ColumnStatisticsTaskSettings/Role")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

/// Move the instant the clock owes every unsettled column statistics task run to `at`, so a case
/// can hold the in-flight window open or make the owed settling due without waiting for it.
fn move_the_run_due_instant(engine: &Arc<engine::InMemoryEngine>, at: SystemTime) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    for run in &mut store.column_statistics_task_runs {
        if run.settles_at.is_some() {
            run.settles_at = Some(at);
        }
    }
}

/// A member of a reported structure read as the string it carries.
fn text(structure: &Value, member: &str) -> String {
    structure
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A numeric member of a reported structure, taking an absent member as the model's own default
/// of 0 — awsJson omits a member whose value equals its modelled default.
fn number(structure: &Value, member: &str) -> f64 {
    structure.get(member).and_then(Value::as_f64).unwrap_or(0.0)
}

/// The `ColumnNameList` a reported run carries, in the order it reports them.
fn reported_columns(run: &Value) -> Vec<String> {
    run.get("ColumnNameList")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The ids of a set of reported runs, so one listing can be compared with another as a set.
fn run_ids(runs: &[Value]) -> BTreeSet<String> {
    runs.iter()
        .map(|run| text(run, "ColumnStatisticsTaskRunId"))
        .collect()
}

/// Assert that `run` has not finished: its status is none of the states a run never leaves
/// again, so the clock still owes it one.
fn assert_in_flight(case: &str, run: &Value) {
    let status = text(run, "Status");
    assert!(
        !TERMINAL_COLUMN_STATISTICS_TASK_RUN_STATES.contains(&status.as_str()),
        "case {case:?}: a run that has just been started still owes a further state, got \
         {status:?} in {run}"
    );
}

/// The error SHAPE an awsJson failure names: the wire code with the namespace it may be
/// qualified with stripped, so the name the model fixes is compared exactly rather than by
/// suffix.
fn error_shape(payload: &Value) -> String {
    let code = error_code(payload);
    code.rsplit('#').next().unwrap_or(code.as_str()).to_string()
}

/// The `message` an awsJson error answer carries.
fn error_message(payload: &Value) -> String {
    payload
        .get("message")
        .or_else(|| payload.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------------
// What a create with no schedule starts, and what that run then decides
// ---------------------------------------------------------------------------

/// A create with no `Schedule` starts the run its settings configure; that run blocks a second
/// create for the table until a stop ends it, and the create that follows the stop starts a run
/// of its own which reads the table's data and succeeds.
#[tokio::test]
async fn registering_settings_without_a_schedule_starts_the_run_they_configure() {
    for (index, row) in registrations().into_iter().enumerate() {
        let case = row.case;
        let (router, engine) = build_router_and_engine();

        let bucket = format!("glue-column-stats-{index}");
        let database = format!("column_stats_db_{index}");
        let table = "orders";
        stage(&router, case, &bucket, row.objects).await;
        store_table(&router, case, &row, &bucket, &database, table).await;

        // Two roles the account really holds, both of which AWS Glue can assume: the one the
        // first create runs as, and the one the create that collides with its run states.
        let first_role = create_role(
            &router,
            case,
            &format!("column-stats-first-{index}"),
            GLUE_SERVICE_PRINCIPAL,
        )
        .await;
        let second_role = create_role(
            &router,
            case,
            &format!("column-stats-second-{index}"),
            GLUE_SERVICE_PRINCIPAL,
        )
        .await;

        // The create a client makes to register the generation of that table's statistics.
        let (status, created) =
            register_settings(&router, &row, &database, table, &first_role).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the create with no Schedule must be served, got {status} with {created}"
        );
        // Nothing has read the run yet, so the status the clock owes it has not been
        // materialised: pushing the due instant out holds the in-flight window open for the
        // assertions below, however slow the host is.
        move_the_run_due_instant(&engine, SystemTime::now() + NOT_YET_DUE);

        // The run the create started: ONE, of that table, carrying what the create configured,
        // and not finished.
        let started = the_only_run_of(&router, case, &database, table).await;
        assert_eq!(
            text(&started, "Role"),
            first_role,
            "case {case:?}: the run reports the Role the create submitted, got {started}"
        );
        assert_in_flight(case, &started);
        assert_eq!(
            reported_columns(&started),
            row.covered
                .iter()
                .map(|(column, _)| (*column).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: the run covers the columns the create configured, got {started}"
        );
        assert_eq!(
            number(&started, "SampleSize"),
            row.sample_size.unwrap_or(0.0),
            "case {case:?}: the run reports the SampleSize the create stated, got {started}"
        );
        let before = run_ids(&runs_of(&router, case, &database, table).await);

        // While that run is in flight a second create for the table is the modelled refusal,
        // with the service's fixed sentence for it.
        let (status, refused) =
            register_settings(&router, &row, &database, table, &second_role).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a create under an in-flight run is refused, got {status} with {refused}"
        );
        assert_eq!(
            error_shape(&refused),
            "ColumnStatisticsTaskRunningException",
            "case {case:?}: expected ColumnStatisticsTaskRunningException, got {refused}"
        );
        assert_eq!(
            error_message(&refused),
            RUN_ALREADY_STARTED_SENTENCE,
            "case {case:?}: expected the sentence AWS words this refusal with, got {refused}"
        );
        assert_eq!(
            COLUMN_STATISTICS_TASK_SETTINGS_RUN_ALREADY_STARTED, RUN_ALREADY_STARTED_SENTENCE,
            "case {case:?}: the constant the handler answers from is that same sentence"
        );
        // The refusal persisted NOTHING: no second run, and the stored settings still run as the
        // role the served create stated.
        assert_eq!(
            run_ids(&runs_of(&router, case, &database, table).await),
            before,
            "case {case:?}: a refused create starts no run, so the run set is unchanged"
        );
        assert_eq!(
            reported_settings_role(&router, &database, table).await,
            Ok(first_role.clone()),
            "case {case:?}: a refused create leaves the stored Role as it was"
        );

        // A stop drives the run the create started to the state a stopped run stands in, which
        // is terminal.
        let (status, stopped) = glue(
            &router,
            REGION,
            "StopColumnStatisticsTaskRun",
            &json!({ "DatabaseName": database, "TableName": table }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the started run is there to stop, got {status} with {stopped}"
        );
        let ended = the_only_run_of(&router, case, &database, table).await;
        assert_eq!(
            text(&ended, "Status"),
            COLUMN_STATISTICS_TASK_RUN_STATE_STOPPED,
            "case {case:?}: a stopped run reports the stopped state, got {ended}"
        );

        // With nothing in flight the create is served again, reconfiguring the generation and
        // starting a run of its own.
        let (status, again) = register_settings(&router, &row, &database, table, &second_role).await;
        assert_eq!(
            status, 200,
            "case {case:?}: with no run in flight the create is served, got {status} with {again}"
        );
        move_the_run_due_instant(&engine, SystemTime::now() + NOT_YET_DUE);
        let runs = runs_of(&router, case, &database, table).await;
        assert_eq!(
            runs.len(),
            2,
            "case {case:?}: the served create started a second run, got {runs:?}"
        );
        let newest = runs[0].clone();
        assert_eq!(
            text(&newest, "Role"),
            second_role,
            "case {case:?}: the newest run runs as the role the second create stated, got {newest}"
        );
        assert_in_flight(case, &newest);
        assert_eq!(
            reported_settings_role(&router, &database, table).await,
            Ok(second_role.clone()),
            "case {case:?}: the served create replaced the stored Role"
        );

        // The run reads the objects behind the table, so once the clock settles it the run
        // succeeded and the statistics it computed are readable.
        move_the_run_due_instant(&engine, SystemTime::now() - ALREADY_DUE);
        let newest_id = text(&newest, "ColumnStatisticsTaskRunId");
        let settled = runs_of(&router, case, &database, table).await;
        let finished = settled
            .iter()
            .find(|run| text(run, "ColumnStatisticsTaskRunId") == newest_id)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the run must still be listed, got {settled:?}")
            });
        assert_eq!(
            text(&finished, "Status"),
            COLUMN_STATISTICS_TASK_RUN_STATE_SUCCEEDED,
            "case {case:?}: a run that read the staged data succeeds, got {finished}"
        );
        assert!(
            finished.get("ErrorMessage").is_none(),
            "case {case:?}: a run that failed at nothing carries no ErrorMessage, got {finished}"
        );

        let (status, statistics) = glue(
            &router,
            REGION,
            "GetColumnStatisticsForTable",
            &json!({
                "DatabaseName": database,
                "TableName": table,
                "ColumnNames": row
                    .covered
                    .iter()
                    .map(|(column, _)| *column)
                    .collect::<Vec<&str>>(),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetColumnStatisticsForTable must answer 200, got {status} with \
             {statistics}"
        );
        assert_eq!(
            statistics
                .get("Errors")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or_default(),
            0,
            "case {case:?}: every covered column has statistics, so nothing is in Errors, got \
             {statistics}"
        );
        let reported = statistics
            .get("ColumnStatisticsList")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (column, statistics_type) in row.covered {
            let entry = reported
                .iter()
                .find(|entry| text(entry, "ColumnName") == *column)
                .cloned()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: {column:?} must carry statistics, got {reported:?}")
                });
            assert_eq!(
                entry
                    .pointer("/StatisticsData/Type")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                *statistics_type,
                "case {case:?}: {column:?} is reported under the statistics category its \
                 declared type maps to, got {entry}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// A create that runs as a role AWS Glue cannot assume
// ---------------------------------------------------------------------------

/// A create whose `Role` the account's own IAM shows AWS Glue cannot assume is the modelled
/// input refusal, and it registers nothing: no settings record and no run at all. Rowed along HOW
/// the role fails to back a run — a role trusting another service, and a role ARN of this account
/// that names no role IAM holds.
#[tokio::test]
async fn a_create_whose_role_glue_cannot_assume_starts_no_run() {
    let unassumable: Vec<(&str, bool)> = vec![
        ("a role whose trust policy admits another service", true),
        ("a role ARN of this account naming no role IAM holds", false),
    ];
    for (index, (case, hold_the_role)) in unassumable.into_iter().enumerate() {
        let (router, engine) = build_router_and_engine();
        let row = Registration {
            case,
            columns: &[("id", "int")],
            objects: &[("rows.csv", "1\n2\n")],
            header_lines: 0,
            column_name_list: None,
            sample_size: None,
            covered: &[("id", "LONG")],
        };
        let bucket = format!("glue-column-stats-role-{index}");
        let database = format!("column_stats_role_db_{index}");
        let table = "orders";
        stage(&router, case, &bucket, row.objects).await;
        store_table(&router, case, &row, &bucket, &database, table).await;

        let role_name = format!("column-stats-unassumable-{index}");
        let role = if hold_the_role {
            // A role the account holds, trusted by a service that is not AWS Glue, so IAM
            // ANSWERS that Glue cannot assume it.
            create_role(&router, case, &role_name, "ec2.amazonaws.com").await
        } else {
            format!("arn:aws:iam::{ACCOUNT}:role/{role_name}")
        };

        let (status, refused) = register_settings(&router, &row, &database, table, &role).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a create that names such a role is refused, got {status} with {refused}"
        );
        assert_eq!(
            error_shape(&refused),
            "InvalidInputException",
            "case {case:?}: expected InvalidInputException, got {refused}"
        );
        assert_eq!(
            error_message(&refused),
            ROLE_NOT_ASSUMABLE,
            "case {case:?}: expected the service's fixed sentence, got {refused}"
        );

        // Nothing was registered: no run to settle, and no settings record to read back.
        move_the_run_due_instant(&engine, SystemTime::now() - ALREADY_DUE);
        assert!(
            runs_of(&router, case, &database, table).await.is_empty(),
            "case {case:?}: a refused create starts no run"
        );
        assert_eq!(
            reported_settings_role(&router, &database, table)
                .await
                .err()
                .map(|code| code.rsplit('#').next().unwrap_or_default().to_string()),
            Some("EntityNotFoundException".to_string()),
            "case {case:?}: a refused create stores no settings record"
        );
    }
}
