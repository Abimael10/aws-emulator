// SPDX-License-Identifier: CC0-1.0

//! A Glue job run's SOURCE READ is authorized as the job's role on the assembled,
//! authenticating emulator, and what the run did is there to read afterwards.
//!
//! The RULE under test: **a job run executes its script as the job's `Role` ("the name or
//! Amazon Resource Name (ARN) of the IAM role associated with this job", `Job$Role`), so every
//! read and write the script makes is authorized against the policies in force on that role —
//! a run whose role may read its source comes to rest `SUCCEEDED` leaving the output its
//! script computed from the objects it really read, and a run whose role may not reach the
//! source path resolves no file there, which its engine reports as the source path not
//! existing, so the run comes to rest `FAILED` carrying that reason and nothing under its
//! output path.** Sources: `Job$Role`, `JobCommand$ScriptLocation` ("the Amazon S3 path to a
//! script that executes a job"), `StartJobRun$Arguments` ("the job arguments associated with
//! this run … that your own job-execution script consumes") and `JobRun$ErrorMessage` ("an
//! error message associated with this job run").
//!
//! Only the assembled router can pin this end of it: the run's script is fetched from Amazon
//! S3 and makes its own reads and writes through S3's own operations over the emulator's
//! in-process callback, the role it runs as is IAM's, and every call below — the client's and
//! the script's — is made against a router that VERIFIES request signatures, which is how the
//! emulator serves traffic. The Glue crate's own `job_run_source_authorization.rs` pins the
//! same rule over a wider table of grant shapes on two hand-picked services with
//! authentication off; what only this suite reaches is the whole-router, authentication-on
//! axis a client of the served emulator drives.
//!
//! The rows span the one thing the rule turns on: whether the policies in force on the run's
//! role reach the source path the run's arguments name.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason `boot_serve.rs` does.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aws_credential_types::Credentials;
use aws_emulator_router::{build_router, InMemoryEngine};
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_provisions_stack_resources::xml_texts;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The XML namespace S3's own model declares, which a client's payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The bucket the script, the source data and the output of this suite live in.
const BUCKET: &str = "xsvc-glue-job-source";

/// The source objects every row reads: three records, so the value a run writes is one only a
/// real read of the source produces.
const SOURCE_CSV: &str = "id,name,value\n1,ada,10\n2,bo,20\n3,cy,30\n";

/// The count of those records, which is what the script writes.
const SOURCE_ROWS: &str = "3";

/// The script both jobs run: it reads the CSV source its `--SOURCE_PATH` names and writes the
/// number of rows that source held under its `--OUTPUT_PATH`.
const SCRIPT: &str = r#"
import sys

from awsglue.utils import getResolvedOptions
from pyspark.context import SparkContext
from pyspark.sql import SparkSession

args = getResolvedOptions(sys.argv, ["SOURCE_PATH", "OUTPUT_PATH"])
spark = SparkSession(SparkContext.getOrCreate())
rows = spark.read.option("header", "true").csv(args["SOURCE_PATH"])
spark.sparkContext.parallelize([str(rows.count())]).saveAsTextFile(args["OUTPUT_PATH"])
"#;

/// The fixed text a source path the run's engine resolved no file at is reported with.
const PATH_DOES_NOT_EXIST: &str = "Path does not exist";

/// The `JobRunState` values a run never leaves again
/// (`smithy_query(query="shape",shape="JobRunState",model_path="services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json")`).
const TERMINAL_RUN_STATES: [&str; 6] = [
    "STOPPED",
    "SUCCEEDED",
    "FAILED",
    "TIMEOUT",
    "ERROR",
    "EXPIRED",
];

/// How long a row waits for its run to come to rest: far above the spans the service owes a
/// run plus the time one script takes. Driven off the STATE the run reports rather than off a
/// fixed sleep, so a slow host delays a row instead of failing it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// One poll of `GetJobRun` inside [`SETTLE_DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// What one call answered.
struct Answer {
    status: u16,
    body: String,
}

impl Answer {
    /// The text of the first `<name>` element of the body, or the empty string.
    fn text(&self, name: &str) -> String {
        xml_texts(&self.body, name)
            .into_iter()
            .next()
            .unwrap_or_default()
    }

    /// The body as JSON, or `Value::Null` for one that is not.
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// The emulator this suite drives, and the credential every call it makes is signed with:
/// the pair this process publishes once request authentication is on.
struct Emulator {
    router: MultiServiceRouter,
    key_id: String,
    secret: String,
}

impl Emulator {
    /// The whole emulator over one engine, serving as it serves traffic — with request
    /// authentication ON, so every call below, and every call a job script's own run makes
    /// over the in-process callback, is verified.
    fn start() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let mut router = build_router(engine);
        router.set_request_authentication(true);
        let published = ::emulator_server::SigV4Authenticator::minted_credentials()
            .expect("turning authentication on publishes this process's keys");
        Self {
            router,
            key_id: published.access_key_id.clone(),
            secret: published.secret_access_key.clone(),
        }
    }

    /// Sign `request` for `signing_name` in [`REGION`], in the `Authorization` header an SDK
    /// request carries.
    fn signed(
        &self,
        mut request: http::Request<Vec<u8>>,
        signing_name: &str,
    ) -> http::Request<Vec<u8>> {
        let identity = Credentials::new(&self.key_id, &self.secret, None, None, "test").into();
        let params = SigningParams::builder()
            .identity(&identity)
            .region(REGION)
            .name(signing_name)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .expect("the signing parameters are complete")
            .into();
        let (method, uri) = (request.method().to_string(), request.uri().to_string());
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().expect("a text header").to_string(),
                )
            })
            .collect();
        let signable = SignableRequest::new(
            &method,
            &uri,
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::Bytes(request.body()),
        )
        .expect("the request is signable");
        let (instructions, _) = sign(signable, &params)
            .expect("the request signs")
            .into_parts();
        instructions.apply_to_request_http1x(&mut request);
        request
    }

    /// Dispatch one signed request as it arrives on the wire.
    async fn send(
        &self,
        method: &str,
        host: &str,
        signing: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Answer {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let built = builder
            .body(body.as_bytes().to_vec())
            .expect("a well-formed request");
        let (parts, payload) = self.signed(built, signing).into_parts();
        let response = self
            .router
            .dispatch(http::Request::from_parts(
                parts,
                bytes::Bytes::from(payload),
            ))
            .await;
        Answer {
            status: response.status().as_u16(),
            body: String::from_utf8_lossy(response.body()).into_owned(),
        }
    }

    /// One Amazon S3 REST call, path-style as the emulator serves it.
    async fn s3(&self, method: &str, path: &str, body: &str) -> Answer {
        let host = format!("s3.{REGION}.amazonaws.com");
        self.send(method, &host, "s3", path, &[], body).await
    }

    /// One IAM `awsQuery` call.
    async fn iam(&self, body: &str) -> Answer {
        self.send(
            "POST",
            "iam.amazonaws.com",
            "iam",
            "/",
            &[("content-type", "application/x-www-form-urlencoded")],
            body,
        )
        .await
    }

    /// One AWS Glue `awsJson1_1` call, which must answer 200.
    async fn glue(&self, case: &str, operation: &str, body: &Value) -> Value {
        let host = format!("glue.{REGION}.amazonaws.com");
        let answered = self
            .send(
                "POST",
                &host,
                "glue",
                "/",
                &[
                    ("content-type", "application/x-amz-json-1.1"),
                    ("x-amz-target", &format!("AWSGlue.{operation}")),
                ],
                &body.to_string(),
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: Glue {operation} must answer 200, got {} with {}",
            answered.status, answered.body
        );
        answered.json()
    }

    /// Poll `GetJobRun` until the run comes to rest, exactly as a client waits for one, and
    /// answer the `JobRun` it then reports.
    async fn run_at_rest(&self, case: &str, job: &str, run_id: &str) -> Value {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let reported = self
                .glue(
                    case,
                    "GetJobRun",
                    &json!({ "JobName": job, "RunId": run_id }),
                )
                .await;
            let run = reported["JobRun"].clone();
            let state = run["JobRunState"].as_str().unwrap_or_default().to_string();
            if TERMINAL_RUN_STATES.contains(&state.as_str()) {
                return run;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the run never came to rest; it reports {run}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Every object key the store holds under `prefix`.
    async fn keys_under(&self, case: &str, prefix: &str) -> Vec<String> {
        let listed = self
            .s3(
                "GET",
                &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
                "",
            )
            .await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: the listing of {prefix} must answer 200, got {} with {}",
            listed.status, listed.body
        );
        xml_texts(&listed.body, "Key")
    }
}

/// One row: what the run's role is allowed, and what its run must come to.
struct Row {
    /// What every assertion of the row reports, and the segment its objects live under.
    case: &'static str,
    /// The `Statement` list of the identity policy in force on the row's role. It carries none
    /// of the characters that are significant in an `application/x-www-form-urlencoded` body,
    /// so the document travels as written.
    statements: &'static str,
    /// Whether the run must succeed and leave the count of the source's rows behind.
    succeeds: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "good-run",
            // "s3:ListBucket" is what a read of the source DIRECTORY needs beside the object
            // reads: the objects under the prefix are listed before they can be read.
            statements: concat!(
                r#"{"Effect":"Allow","Action":["s3:GetObject","s3:PutObject"],"#,
                r#""Resource":"arn:aws:s3:::xsvc-glue-job-source/*"},"#,
                r#"{"Effect":"Allow","Action":"s3:ListBucket","#,
                r#""Resource":"arn:aws:s3:::xsvc-glue-job-source"}"#
            ),
            succeeds: true,
        },
        Row {
            // The role was given the script read and the output write and never the source
            // read: nothing denies the source, no statement allows it.
            case: "bad-run",
            statements: concat!(
                r#"{"Effect":"Allow","Action":"s3:GetObject","#,
                r#""Resource":"arn:aws:s3:::xsvc-glue-job-source/scripts/*"},"#,
                r#"{"Effect":"Allow","Action":"s3:PutObject","#,
                r#""Resource":"arn:aws:s3:::xsvc-glue-job-source/output/*"},"#,
                r#"{"Effect":"Allow","Action":"s3:ListBucket","#,
                r#""Resource":"arn:aws:s3:::xsvc-glue-job-source"}"#
            ),
            succeeds: false,
        },
    ]
}

/// The trust document a Glue job's role carries: Glue assumes it to perform the run.
const GLUE_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"glue.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

#[test]
fn a_job_runs_source_read_is_authorized_as_the_jobs_role() {
    std::thread::Builder::new()
        .name("xsvc-glue-job-source".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let emulator = Emulator::start();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(2)
                .build()
                .expect("a multi-thread runtime");

            runtime.block_on(async {
                let created = emulator
                    .s3(
                        "PUT",
                        &format!("/{BUCKET}"),
                        &format!(
                            "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
                             <LocationConstraint>{REGION}</LocationConstraint>\
                             </CreateBucketConfiguration>"
                        ),
                    )
                    .await;
                assert_eq!(
                    created.status, 200,
                    "setup: the bucket must be created, got {} with {}",
                    created.status, created.body
                );
                for (key, body) in [
                    ("data/input.csv", SOURCE_CSV),
                    ("scripts/read_csv.py", SCRIPT),
                ] {
                    let put = emulator
                        .s3("PUT", &format!("/{BUCKET}/{key}"), body)
                        .await;
                    assert_eq!(
                        put.status, 200,
                        "setup: {key} must be written, got {} with {}",
                        put.status, put.body
                    );
                }

                for row in rows() {
                    let case = row.case;
                    let role_name = format!("glue-job-{case}");
                    let role = emulator
                        .iam(&format!(
                            "Action=CreateRole&Version=2010-05-08&RoleName={role_name}\
                             &AssumeRolePolicyDocument={GLUE_TRUST}"
                        ))
                        .await;
                    assert_eq!(
                        role.status, 200,
                        "setup {case:?}: the role must be created, got {} with {}",
                        role.status, role.body
                    );
                    let role_arn = role.text("Arn");
                    assert!(
                        !role_arn.is_empty(),
                        "setup {case:?}: CreateRole must answer the role's ARN, got {}",
                        role.body
                    );
                    let granted = emulator
                        .iam(&format!(
                            "Action=PutRolePolicy&Version=2010-05-08&RoleName={role_name}\
                             &PolicyName=source-access&PolicyDocument=\
                             {{\"Version\":\"2012-10-17\",\"Statement\":[{}]}}",
                            row.statements
                        ))
                        .await;
                    assert_eq!(
                        granted.status, 200,
                        "setup {case:?}: the policy must be stored, got {} with {}",
                        granted.status, granted.body
                    );

                    let job = format!("glue-source-{case}");
                    emulator
                        .glue(
                            case,
                            "CreateJob",
                            &json!({
                                "Name": job,
                                "Role": role_arn,
                                "Command": {
                                    "Name": "glueetl",
                                    "ScriptLocation": format!("s3://{BUCKET}/scripts/read_csv.py"),
                                    "PythonVersion": "3",
                                },
                                "GlueVersion": "4.0",
                                "WorkerType": "G.1X",
                                "NumberOfWorkers": 2,
                            }),
                        )
                        .await;

                    let source = format!("s3://{BUCKET}/data/");
                    let output = format!("s3://{BUCKET}/output/{case}/");
                    let started = emulator
                        .glue(
                            case,
                            "StartJobRun",
                            &json!({
                                "JobName": job,
                                "Arguments": {
                                    "--SOURCE_PATH": source,
                                    "--OUTPUT_PATH": output,
                                },
                            }),
                        )
                        .await;
                    let run_id = started["JobRunId"]
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("case {case:?}: StartJobRun must answer a JobRunId, got {started}")
                        })
                        .to_string();

                    let run = emulator.run_at_rest(case, &job, &run_id).await;
                    let state = run["JobRunState"].as_str().unwrap_or_default().to_string();
                    let message = run["ErrorMessage"].as_str().unwrap_or_default().to_string();
                    let written = emulator
                        .keys_under(case, &format!("output/{case}/"))
                        .await;

                    if row.succeeds {
                        assert_eq!(
                            state, "SUCCEEDED",
                            "case {case:?}: expected SUCCEEDED, got {run}"
                        );
                        let part = written
                            .iter()
                            .find(|key| {
                                key.rsplit('/').next().unwrap_or_default().starts_with("part-")
                            })
                            .unwrap_or_else(|| {
                                panic!(
                                    "case {case:?}: expected a part object under the output \
                                     path, got {written:?}"
                                )
                            });
                        let body = emulator.s3("GET", &format!("/{BUCKET}/{part}"), "").await;
                        assert_eq!(
                            body.body.trim(),
                            SOURCE_ROWS,
                            "case {case:?}: expected the count of the source's rows in {part}, \
                             got {:?}",
                            body.body
                        );
                    } else {
                        assert_eq!(
                            state, "FAILED",
                            "case {case:?}: expected FAILED, got {run}"
                        );
                        assert!(
                            message.contains(PATH_DOES_NOT_EXIST) && message.contains(&source),
                            "case {case:?}: expected an ErrorMessage carrying \
                             {PATH_DOES_NOT_EXIST:?} and the source {source:?}, got {message:?}"
                        );
                        assert!(
                            written.is_empty(),
                            "case {case:?}: a run that could not read its source wrote no \
                             output, got {written:?}"
                        );
                    }
                }
            });
        })
        .expect("spawn the glue job source thread")
        .join()
        .expect("a job run's source read is authorized as the job's role");
}
