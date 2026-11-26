// SPDX-License-Identifier: CC0-1.0

//! An EMR Serverless Spark run that writes a TABLE reaches the AWS Glue Data Catalog as its own
//! execution role, and the run's DRIVER LOG carries what that workload did — including the
//! catalog's own refusal when the role may not make the call.
//!
//! The RULE under test: **a Spark program's `saveAsTable` registers the table in the data
//! catalog through the catalog's own API, as the identity the work runs as, so the Data Catalog
//! authorizes that call against the policies in force on the run's `executionRoleArn` exactly as
//! it authorizes any other client's — a run whose role carries the grant registers a table a
//! later `GetTable` really reads, and a run whose role is missing the grant for one catalog
//! action FAILS with that catalog refusal, whose class (`AccessDeniedException`) and sentence
//! reach the run's driver log, because the driver log is the driver's own standard error and not
//! a narration of the service's states.**
//!
//! Only a cross-service suite can pin this: EMR Serverless runs the program, IAM holds the role
//! and the policies the decision is taken from, STS mints the session the run's calls are signed
//! as, AWS Glue holds the catalog the program writes into and authorizes the write, Amazon S3
//! holds the program, the data it writes and the log artifact the run publishes, and CloudWatch
//! Logs holds that same driver log as the events a reader fetches line by line. It therefore
//! runs against the WHOLE `build_router` service set, as the binary serves it, and every
//! assertion reads what one of those services persisted — never anything this suite handed
//! another.
//!
//! The rows differ along what the rule generalises over: WHICH catalog action the role is missing
//! (the write the registration makes, and the read it makes first), the FORMAT the table is
//! written in (Apache Parquet and delimited text), the LAYOUT (partitioned by a column, and not
//! partitioned at all) and WHERE the table's location comes from (the write's own `path` option,
//! and the database's own `LocationUri`). A second suite drives the other half of the same
//! writer: what each SAVE MODE means for a table that is already registered.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for: the one whose `CreateBucket` needs no location
/// constraint in its body.
const REGION: &str = "us-east-1";

/// The bucket every program of this suite is fetched from, writes its tables into and publishes
/// its driver log to.
const BUCKET: &str = "emr-serverless-saveastable";

/// The service principal EMR Serverless assumes a job run's execution role as.
const EMR_SERVERLESS_PRINCIPAL: &str = "emr-serverless.amazonaws.com";

/// The Amazon EMR release the applications of this suite run on.
const RELEASE_LABEL: &str = "emr-7.1.0";

/// The `JobRunState` values a run never leaves. Written out here rather than imported from the
/// service crate, so a wrong catalog in `src/` cannot make this suite pass. Source: the
/// `JobRunState` enum (`SUBMITTED`, `PENDING`, `SCHEDULED`, `RUNNING`, `SUCCESS`, `FAILED`,
/// `CANCELLING`, `CANCELLED`, `QUEUED`).
const TERMINAL_STATES: [&str; 3] = ["SUCCESS", "FAILED", "CANCELLED"];

/// The worker type a Spark application's driver runs as, which is the key segment the run's log
/// artifact is published under (recorded behaviour; the model states no key scheme).
const SPARK_DRIVER: &str = "SPARK_DRIVER";

/// The artifact a finished run publishes under that segment.
const STDERR_OBJECT: &str = "stderr.gz";

/// The log type the driver's standard error is delivered to CloudWatch Logs under. Source:
/// `CloudWatchLoggingConfiguration$logTypes`, whose valid values are `STDOUT`, `STDERR`,
/// `HIVE_LOG`, `TEZ_AM` and `SYSTEM_LOGS`.
const STDERR_LOG_TYPE: &str = "STDERR";

/// The CloudWatch Logs group EMR Serverless publishes a job run's logs to when the
/// configuration names none, as its own logging documentation gives it.
const DEFAULT_LOG_GROUP: &str = "/aws/emr-serverless";

/// How long a poll waits for a job run to come to REST: the stages the clock owes it, the
/// execution of its own program and the span the state that execution answered falls due after,
/// with room to spare. The wait is on the CONDITION, so a loaded host delays a row instead of
/// breaking it.
const DEADLINE: Duration = Duration::from_secs(180);

/// How long a poll sleeps between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The Glue catalog actions the Spark metastore client of a run that writes a partitioned table
/// makes, each one withheld from a row's role to see what the run then reports.
///
/// Source: the actions the Data Catalog publishes for those calls (`glue:GetDatabase`,
/// `glue:GetTable`, `glue:CreateTable`, `glue:UpdateTable`, `glue:BatchCreatePartition`,
/// `glue:GetPartitions`), which are what an EMR Serverless execution role is scoped with.
const CATALOG_ACTIONS: [&str; 6] = [
    "glue:GetDatabase",
    "glue:GetTable",
    "glue:CreateTable",
    "glue:UpdateTable",
    "glue:BatchCreatePartition",
    "glue:GetPartitions",
];

/// The whole emulator over one engine, exactly as `core/router/src/lib.rs` assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: the router above runs with authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One EMR Serverless call (`restJson1`, REST over `/applications/...`): its status and body.
async fn emr_serverless(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("emr-serverless.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("emr-serverless"))
        .header("content-type", "application/json")
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The same, for a call that must be served.
async fn emr_serverless_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = emr_serverless(router, method, path, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {method} {path}, got {status} with {parsed}"
    );
    parsed
}

/// One AWS Glue call (`awsJson1_1`, `AWSGlue.<Operation>`): its status and body.
async fn glue(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let host = format!("glue.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSGlue.{operation}"))
        .header("authorization", authorization("glue"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The same, for a call that must be served.
async fn glue_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = glue(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Glue {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call (`awsJson1_1`, `Logs_20140328.<Operation>`): its status and body.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let host = format!("logs.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header("authorization", authorization("logs"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One IAM `awsQuery` call, which must answer 200.
async fn iam(router: &MultiServiceRouter, case: &str, body: &str) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("authorization", authorization("iam"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body.to_owned()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from IAM, got {status} with {text}"
    );
}

/// One path-style Amazon S3 request: its status and body.
async fn s3(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .body(Bytes::copy_from_slice(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.body().to_vec())
}

/// Put `body` at `key`, which must be served.
async fn put_object(router: &MultiServiceRouter, case: &str, key: &str, body: &[u8]) {
    let (status, answered) = s3(router, "PUT", &format!("/{BUCKET}/{key}"), body).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: PutObject {key} must succeed, got {status} {}",
        String::from_utf8_lossy(&answered)
    );
}

/// The keys of every object under `prefix`, from `ListObjectsV2`.
async fn keys_under(router: &MultiServiceRouter, case: &str, prefix: &str) -> Vec<String> {
    let (status, body) = s3(
        router,
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
    )
    .await;
    let listing = String::from_utf8_lossy(&body).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: listing {prefix} must succeed, got {status} with {listing}"
    );
    listing
        .split("<Key>")
        .skip(1)
        .filter_map(|rest| rest.split_once("</Key>").map(|(key, _)| key.to_string()))
        .collect()
}

/// A trust policy admitting `principal` and nothing else, in the form an `awsQuery` form body
/// carries it: its quotes and spaces percent-encoded.
fn trust_policy(principal: &str) -> String {
    form_encoded(&format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    ))
}

/// One policy document in the form an `awsQuery` form body carries it.
fn form_encoded(document: &str) -> String {
    document
        .replace('%', "%25")
        .replace('"', "%22")
        .replace(' ', "%20")
        .replace('[', "%5B")
        .replace(']', "%5D")
        .replace('{', "%7B")
        .replace('}', "%7D")
        .replace('\n', "%0A")
        .replace('+', "%2B")
        .replace('&', "%26")
}

/// The identity policy a row's execution role carries: every object-store action the run's own
/// reads and writes make on this suite's bucket, and every catalog action its metastore client
/// makes EXCEPT `withheld` — so the withheld action is the only difference between the rows.
fn execution_policy(withheld: Option<&str>) -> String {
    let granted: Vec<String> = CATALOG_ACTIONS
        .iter()
        .filter(|action| Some(**action) != withheld)
        .map(|action| format!("\"{action}\""))
        .collect();
    form_encoded(&format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[\
         {{\"Effect\":\"Allow\",\"Action\":[\"s3:GetObject\",\"s3:PutObject\",\
         \"s3:DeleteObject\",\"s3:ListBucket\"],\
         \"Resource\":[\"arn:aws:s3:::{BUCKET}\",\"arn:aws:s3:::{BUCKET}/*\"]}},\
         {{\"Effect\":\"Allow\",\"Action\":[{}],\"Resource\":\"*\"}}]}}",
        granted.join(",")
    ))
}

/// Create the role `name`, trusted by EMR Serverless and carrying [`execution_policy`], and
/// answer its ARN.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    withheld: Option<&str>,
) -> String {
    iam(
        router,
        case,
        &format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={name}&AssumeRolePolicyDocument={}",
            trust_policy(EMR_SERVERLESS_PRINCIPAL)
        ),
    )
    .await;
    iam(
        router,
        case,
        &format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName={name}\
             &PolicyName=catalog-and-objects&PolicyDocument={}",
            execution_policy(withheld)
        ),
    )
    .await;
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// The program every row submits: it computes a dataset of its own and writes it as a TABLE of
/// the catalog, under the format, layout and location its command line names.
///
/// It writes one line to its own standard ERROR, which is the stream a Spark driver's log is: a
/// row reads that line back out of the published artifact, so what the log carries is the
/// driver's own output and not a narration of it.
const SAVE_AS_TABLE_PROGRAM: &str = r#"
import sys

from pyspark.sql import SparkSession

spark = SparkSession.builder.appName("saveastable").enableHiveSupport().getOrCreate()
database, table, location, kind, partitioned = sys.argv[1:6]
frame = spark.createDataFrame(
    [("ana", 3, "2026-01-01"), ("bo", 5, "2026-01-02"), ("cy", 7, "2026-01-01")],
    "name string, amount int, dt string",
)
writer = frame.write.mode("overwrite").format(kind)
if location != "-":
    writer = writer.option("path", location)
if partitioned == "yes":
    writer = writer.partitionBy("dt")
writer.saveAsTable("%s.%s" % (database, table))
sys.stderr.write("driver: registered %s.%s\n" % (database, table))
spark.stop()
"#;

/// One row: which catalog action the role is missing, and what the run has to come to.
struct Row {
    case: &'static str,
    /// The catalog action withheld from the role, or `None` for a role carrying every one.
    withheld: Option<&'static str>,
    /// The format the table is written in.
    kind: &'static str,
    /// Whether the write partitions the table by its `dt` column.
    partitioned: bool,
    /// Whether the write names the table's location itself (`path`), or leaves it to the
    /// database's own `LocationUri`.
    names_location: bool,
}

/// The rows the rule is asserted over. See the suite docs for what they span.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "every catalog action granted, a partitioned Parquet table",
            withheld: None,
            kind: "parquet",
            partitioned: true,
            names_location: true,
        },
        Row {
            case: "the registration's own write withheld",
            withheld: Some("glue:CreateTable"),
            kind: "parquet",
            partitioned: true,
            names_location: true,
        },
        Row {
            case: "the read the registration makes first withheld",
            withheld: Some("glue:GetDatabase"),
            kind: "parquet",
            partitioned: true,
            names_location: true,
        },
        Row {
            case: "every action granted, delimited text laid out in the database's location",
            withheld: None,
            kind: "csv",
            partitioned: false,
            names_location: false,
        },
    ]
}

/// Poll `GetJobRun` until the run reports a state it never leaves, and answer that read.
async fn poll_until_terminal(
    router: &MultiServiceRouter,
    case: &str,
    application_id: &str,
    job_run_id: &str,
) -> Value {
    let path = format!("/applications/{application_id}/jobruns/{job_run_id}");
    let deadline = Instant::now() + DEADLINE;
    loop {
        let answer = emr_serverless_ok(router, case, "GET", &path, &json!({})).await;
        let state = answer["jobRun"]["state"].as_str().unwrap_or_default().to_string();
        if TERMINAL_STATES.contains(&state.as_str()) {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job run must reach a terminal state, last saw {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll the run's CloudWatch Logs stream until its driver log is there, and answer that log as
/// its text: the events of the stream, one per line, which is the same log the Amazon S3
/// artifact carries.
async fn poll_until_delivered(
    router: &MultiServiceRouter,
    case: &str,
    stream: &str,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, answered) = logs(
            router,
            "GetLogEvents",
            &json!({
                "logGroupName": DEFAULT_LOG_GROUP,
                "logStreamName": stream,
                "startFromHead": true,
            }),
        )
        .await;
        let lines: Vec<String> = answered["events"]
            .as_array()
            .map(|events| {
                events
                    .iter()
                    .filter_map(|event| event["message"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if status == 200 && !lines.is_empty() {
            return lines.join("\n");
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the run must deliver its driver log to {stream}, last read {status} \
             with {answered}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll the Amazon S3 destination until the run's own driver-log ARTIFACT is there, and answer
/// its key: the object is a gzip stream, and what this suite reads of its text it reads from the
/// same log delivered as events ([`poll_until_delivered`]).
async fn poll_until_published(router: &MultiServiceRouter, case: &str, key: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, _body) = s3(router, "HEAD", &format!("/{BUCKET}/{key}"), &[]).await;
        if status == 200 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the run must publish its driver log at {key}, last read {status}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What one submitted run came to: the state it rests in, the `stateDetails` it carries, and the
/// driver log it published.
struct Ran {
    state: String,
    details: String,
    log: String,
}

/// Submit `program` to an application of its own, as a role missing `withheld` (or carrying
/// every catalog action for `None`), wait for the run to come to rest, and answer that with the
/// driver log it published.
///
/// `ordinal` names the row's own role, program object, application and client tokens, so each
/// row of this suite stands alone. Both destinations the application configures are read: the
/// gzip ARTIFACT in the object store is asserted to be there, and its text is read from the same
/// log delivered as the events of a CloudWatch Logs stream.
async fn ran(
    router: &MultiServiceRouter,
    case: &str,
    ordinal: &str,
    program: &str,
    arguments: &[String],
    withheld: Option<&str>,
) -> Ran {
    let role = given_role(router, case, &format!("saveastable-role-{ordinal}"), withheld).await;
    let program_key = format!("programs/{ordinal}/save_as_table.py");
    put_object(router, case, &program_key, program.as_bytes()).await;
    let application = emr_serverless_ok(
        router,
        case,
        "POST",
        "/applications",
        &json!({
            "name": format!("saveastable-{ordinal}"),
            "clientToken": format!("application-{ordinal}"),
            "type": "SPARK",
            "releaseLabel": RELEASE_LABEL,
            "monitoringConfiguration": {
                "s3MonitoringConfiguration": { "logUri": format!("s3://{BUCKET}/logs/") },
                "cloudWatchLoggingConfiguration": { "enabled": true },
            },
        }),
    )
    .await;
    let application_id = application["applicationId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateApplication must answer an id"))
        .to_string();
    let started = emr_serverless_ok(
        router,
        case,
        "POST",
        &format!("/applications/{application_id}/jobruns"),
        &json!({
            "clientToken": format!("jobrun-{ordinal}"),
            "executionRoleArn": role,
            "jobDriver": {
                "sparkSubmit": {
                    "entryPoint": format!("s3://{BUCKET}/{program_key}"),
                    "entryPointArguments": arguments,
                },
            },
        }),
    )
    .await;
    let job_run_id = started["jobRunId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartJobRun must answer an id"))
        .to_string();
    let answer = poll_until_terminal(router, case, &application_id, &job_run_id).await;
    poll_until_published(
        router,
        case,
        &format!(
            "logs/applications/{application_id}/jobs/{job_run_id}/{SPARK_DRIVER}/{STDERR_OBJECT}"
        ),
    )
    .await;
    let log = poll_until_delivered(
        router,
        case,
        &format!(
            "applications/{application_id}/jobs/{job_run_id}/{SPARK_DRIVER}/{STDERR_LOG_TYPE}"
        ),
    )
    .await;
    Ran {
        state: answer["jobRun"]["state"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        details: answer["jobRun"]["stateDetails"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        log,
    }
}

/// RULE: see the suite docs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spark_run_registers_its_table_as_its_role_and_logs_the_catalogs_refusal() {
    let router = router();
    let (status, body) = s3(&router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "setup: CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let database = format!("sim2real_db_{index}");
        let table = format!("sales_{index}");
        let warehouse = format!("s3://{BUCKET}/warehouse/{index}");
        let location = format!("{warehouse}/{table}");
        // The database the program writes into, created through the catalog's own API and
        // carrying the location its tables are laid out under.
        glue_ok(
            &router,
            case,
            "CreateDatabase",
            &json!({
                "DatabaseInput": { "Name": database, "LocationUri": warehouse },
            }),
        )
        .await;
        let answered = ran(
            &router,
            case,
            &index.to_string(),
            SAVE_AS_TABLE_PROGRAM,
            &[
                database.clone(),
                table.clone(),
                if row.names_location {
                    location.clone()
                } else {
                    "-".to_string()
                },
                row.kind.to_string(),
                if row.partitioned { "yes" } else { "no" }.to_string(),
            ],
            row.withheld,
        )
        .await;
        let state = answered.state.as_str();
        let details = answered.details.as_str();
        let log = answered.log;
        let read_back = glue(
            &router,
            "GetTable",
            &json!({ "DatabaseName": database, "Name": table }),
        )
        .await;
        match row.withheld {
            // A role carrying every catalog action registers the table: the run succeeds, the
            // catalog really holds the table the program wrote — with the columns, the
            // partition keys and the location that write gave it — and the driver's own output
            // is in the published log.
            None => {
                assert_eq!(
                    state, "SUCCESS",
                    "case {case:?}: expected the run to succeed, got {state} / {details:?} with \
                     the driver log {log:?}"
                );
                assert!(
                    log.contains(&format!("driver: registered {database}.{table}")),
                    "case {case:?}: the driver log must carry the driver's own output, got {log:?}"
                );
                let (status, answered) = read_back;
                assert_eq!(
                    status, 200,
                    "case {case:?}: GetTable must read the registered table back, got {status} \
                     with {answered}"
                );
                let stored = &answered["Table"];
                let expected_location = if row.names_location {
                    location.clone()
                } else {
                    format!("{warehouse}/{table}")
                };
                assert_eq!(
                    stored["StorageDescriptor"]["Location"].as_str(),
                    Some(expected_location.as_str()),
                    "case {case:?}: expected the table's location to be \
                     {expected_location:?}, got {answered}"
                );
                assert_eq!(
                    stored["Parameters"]["classification"].as_str(),
                    Some(row.kind),
                    "case {case:?}: expected the table's classification to be {:?}, got \
                     {answered}",
                    row.kind
                );
                let columns: Vec<&str> = stored["StorageDescriptor"]["Columns"]
                    .as_array()
                    .map(|held| {
                        held.iter()
                            .filter_map(|column| column["Name"].as_str())
                            .collect()
                    })
                    .unwrap_or_default();
                let keys: Vec<&str> = stored["PartitionKeys"]
                    .as_array()
                    .map(|held| {
                        held.iter()
                            .filter_map(|column| column["Name"].as_str())
                            .collect()
                    })
                    .unwrap_or_default();
                if row.partitioned {
                    assert_eq!(
                        (columns.clone(), keys.clone()),
                        (vec!["name", "amount"], vec!["dt"]),
                        "case {case:?}: a partitioned table holds its partition column as a \
                         partition key, got {answered}"
                    );
                    // The partitions the write laid out are registered on the table, which is
                    // what the catalog reports for it.
                    let partitions = glue_ok(
                        &router,
                        case,
                        "GetPartitions",
                        &json!({ "DatabaseName": database, "TableName": table }),
                    )
                    .await;
                    let mut values: Vec<String> = partitions["Partitions"]
                        .as_array()
                        .map(|held| {
                            held.iter()
                                .filter_map(|partition| {
                                    partition["Values"][0].as_str().map(str::to_string)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    values.sort();
                    assert_eq!(
                        values,
                        vec!["2026-01-01".to_string(), "2026-01-02".to_string()],
                        "case {case:?}: expected one partition per value the dataset holds, got \
                         {partitions}"
                    );
                } else {
                    assert_eq!(
                        (columns.clone(), keys.clone()),
                        (vec!["name", "amount", "dt"], Vec::<&str>::new()),
                        "case {case:?}: an unpartitioned table holds every column and no \
                         partition key, got {answered}"
                    );
                }
                // The DATA is really there: the objects the write left under the table's own
                // location, read through the object store's own listing.
                let prefix = expected_location
                    .trim_start_matches(&format!("s3://{BUCKET}/"))
                    .to_string();
                let keys = keys_under(&router, case, &prefix).await;
                assert!(
                    keys.iter().any(|key| key.contains("part-")),
                    "case {case:?}: expected the table's own part files under {prefix}, got \
                     {keys:?}"
                );
            }
            // A role missing ONE catalog action makes that call and is refused: the run fails,
            // the refusal's own class and sentence are in the driver log, and the catalog holds
            // no table for a registration it refused.
            Some(action) => {
                assert_eq!(
                    state, "FAILED",
                    "case {case:?}: expected the run to fail, got {state} / {details:?} with the \
                     driver log {log:?}"
                );
                assert!(
                    log.contains("AccessDeniedException"),
                    "case {case:?}: the driver log must name the catalog's refusal class, got \
                     {log:?}"
                );
                assert!(
                    log.contains(&format!("is not authorized to perform: {action}")),
                    "case {case:?}: the driver log must carry the catalog's own refusal of \
                     {action}, got {log:?}"
                );
                assert!(
                    details.contains("AccessDeniedException"),
                    "case {case:?}: the run's stateDetails must name what failed it, got \
                     {details:?}"
                );
                let (status, answered) = read_back;
                assert_eq!(
                    status, 400,
                    "case {case:?}: the catalog must hold no table for a refused registration, \
                     got {status} with {answered}"
                );
                assert!(
                    answered["__type"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("EntityNotFoundException"),
                    "case {case:?}: expected the table to be absent, got {answered}"
                );
            }
        }
    }
}

/// The program the save-mode row submits: it writes the same table FOUR times over, once per
/// save mode, and reports on its own standard error what each one did.
///
/// "Specifies the behavior when data or table already exists" (`DataFrameWriter.mode`), so what
/// each mode means is only observable against a table that is ALREADY there: the first write
/// creates it, `append` adds its rows to it, the default refuses it, `ignore` leaves it alone,
/// `overwrite` replaces its data, and a mode Spark has none of is refused rather than read as
/// one of them. Each row of the dataset carries the write that produced it, so the objects the
/// table holds afterwards say which writes are in it.
const SAVE_MODES_PROGRAM: &str = r#"
import sys

from pyspark.sql import SparkSession

spark = SparkSession.builder.appName("savemodes").enableHiveSupport().getOrCreate()
database, table = sys.argv[1:3]
name = "%s.%s" % (database, table)


def written(tag, count):
    return spark.createDataFrame(
        [(tag, value) for value in range(count)], "tag string, value int"
    )


def report(label, action):
    try:
        action()
        sys.stderr.write("driver: %s wrote\n" % (label,))
    except BaseException as refused:
        sys.stderr.write("driver: %s refused %s\n" % (label, type(refused).__name__))


# The table itself, created by the first write.
written("created", 2).coalesce(1).write.format("csv").saveAsTable(name)
sys.stderr.write("driver: created\n")
# ... then one write per mode against the table that is now there.
report("errorifexists", lambda: written("second", 1).write.format("csv").saveAsTable(name))
report(
    "ignore",
    lambda: written("ignored", 1).coalesce(1).write.mode("ignore").format("csv").saveAsTable(name),
)
report(
    "append",
    lambda: written("appended", 1).coalesce(1).write.mode("append").format("csv").saveAsTable(name),
)
report("unknown", lambda: written("typo", 1).write.mode("overwite").format("csv").saveAsTable(name))
report(
    "overwrite",
    lambda: written("overwritten", 3)
    .coalesce(1)
    .write.mode("overwrite")
    .format("csv")
    .saveAsTable(name),
)
spark.stop()
"#;

/// RULE: a save mode decides what a table ALREADY THERE means — `append` adds this dataset's
/// rows to it, the default refuses it, `ignore` leaves it alone and `overwrite` replaces its
/// data and its registered definition — and a mode Spark has none of is refused rather than
/// read as any of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_save_mode_decides_what_a_table_already_there_means() {
    let case = "a table written once per save mode";
    let router = router();
    let (status, body) = s3(&router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "setup: CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    let database = "sim2real_db_modes";
    let table = "sales_modes";
    let warehouse = format!("s3://{BUCKET}/warehouse/modes");
    glue_ok(
        &router,
        case,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": database, "LocationUri": warehouse } }),
    )
    .await;
    let answered = ran(
        &router,
        case,
        "modes",
        SAVE_MODES_PROGRAM,
        &[database.to_string(), table.to_string()],
        None,
    )
    .await;
    assert_eq!(
        answered.state, "SUCCESS",
        "case {case:?}: expected the run to succeed, got {} / {:?} with the driver log {:?}",
        answered.state, answered.details, answered.log
    );
    // What each mode DID, read off the driver's own log: a mode that refuses says which class
    // refused it, and a mode that writes says it wrote.
    for held in [
        "driver: created",
        // The default refuses a table that is already there, as Spark's analysis refuses it.
        "driver: errorifexists refused AnalysisException",
        // `ignore` leaves the table alone, and that is not a failure.
        "driver: ignore wrote",
        "driver: append wrote",
        // A mode Spark has none of is refused, and nothing of the table is touched.
        "driver: unknown refused ValueError",
        "driver: overwrite wrote",
    ] {
        assert!(
            answered.log.contains(held),
            "case {case:?}: expected the driver log to carry {held:?}, got {:?}",
            answered.log
        );
    }
    // The table is still ONE registration, now carrying the overwrite's own definition.
    let registered = glue_ok(
        &router,
        case,
        "GetTable",
        &json!({ "DatabaseName": database, "Name": table }),
    )
    .await;
    let location = registered["Table"]["StorageDescriptor"]["Location"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        location,
        format!("{warehouse}/{table}"),
        "case {case:?}: expected the table to keep its location, got {registered}"
    );
    // ... and its DATA is the overwrite's alone: the rows the first write and the append left
    // are gone, which is what overwriting a table means.
    let prefix = location
        .trim_start_matches(&format!("s3://{BUCKET}/"))
        .to_string();
    let keys = keys_under(&router, case, &prefix).await;
    let parts: Vec<String> = keys
        .iter()
        .filter(|key| key.contains("part-"))
        .cloned()
        .collect();
    assert_eq!(
        parts.len(),
        1,
        "case {case:?}: expected the overwrite's one part file under {prefix}, got {keys:?}"
    );
    let (status, body) = s3(&router, "GET", &format!("/{BUCKET}/{}", parts[0]), &[]).await;
    let held = String::from_utf8_lossy(&body).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: GetObject on {} must succeed, got {status}",
        parts[0]
    );
    assert!(
        held.contains("overwritten") && !held.contains("appended") && !held.contains("created"),
        "case {case:?}: expected the table to hold the overwrite's rows alone, got {held:?}"
    );
}
