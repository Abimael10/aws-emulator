// SPDX-License-Identifier: CC0-1.0

//! An EMR Serverless job run RUNS the program its driver submitted, end to end over the whole
//! assembled router.
//!
//! THE RULE: a job run's end is the outcome of running the work its driver named. "A job run
//! is a unit of work, such as a Spark JAR, Hive query, or SparkSQL query, that you submit to
//! an Amazon EMR Serverless application" (`JobRun` doc), and the modelled lifecycle takes it
//! through `RUNNING` ("`startedAt`: the date and time when the job moved to the RUNNING
//! state") to a state it never leaves — so a `sparkSubmit` of a Python application in Amazon
//! S3 is really executed: a program that exits cleanly reaches `SUCCESS` with the objects it
//! wrote readable afterwards, and one that raises or exits non-zero reaches the modelled
//! failure state carrying its OWN error. A run that terminated then reports what it took:
//! `startedAt <= endedAt`, `totalExecutionDurationSeconds` derived from that span
//! ("only available for job runs in a COMPLETED, FAILED, or CANCELLED state"),
//! `queuedDurationMilliseconds` for the time it spent `QUEUED`, and the two utilisation
//! structures — the aggregate "from the time the job starts to execute, until the time the job
//! terminates" and the billed one, which "include a 1-minute minimum usage for workers, plus
//! additional storage over 20 GB per worker".
//!
//! Only a cross-service suite can pin this: the program that runs is an object a client put
//! in Amazon S3 with `PutObject`, and the objects the program writes are read back with
//! `GetObject` — never through anything this suite handed EMR Serverless. It therefore runs
//! against the WHOLE `build_router` service set, with every call made the way a client makes
//! it.
//!
//! The rows differ along what the rule generalises over: what the program comes to (it writes
//! and exits cleanly, it raises, it exits with a status of its own), which Spark surface it
//! computes through, how many partitions the work is sliced into, and how the submit sizes the
//! driver it is measured over.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for: the one whose `CreateBucket` needs no
/// location constraint in its body.
const REGION: &str = "us-east-1";

/// The bucket every program of this suite is read from and every output is written to.
const BUCKET: &str = "emr-serverless-job-programs";

/// The execution role a job run is submitted with, satisfying `IAMRoleArn`'s pattern.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/EMRServerlessJobRole";

/// The release the application runs on, and the engine it serves.
const RELEASE_LABEL: &str = "emr-7.1.0";
const ENGINE_TYPE: &str = "SPARK";

/// The `JobRunState` values a run never leaves, as the model's own `JobRunState` enum lists
/// them — written out here rather than imported from the service crate, so a wrong constant in
/// the implementation cannot make this suite pass.
const TERMINAL_STATES: [&str; 3] = ["SUCCESS", "FAILED", "CANCELLED"];

/// The `ApplicationState` an application holding capacity is in.
const STARTED: &str = "STARTED";

/// How long a poll waits for the condition it is waiting on: every stage the clock owes an
/// application and a job run, plus the whole span one run of a program may take, with room to
/// spare on a loaded host. The wait is on the CONDITION — it looks again until it holds — so a
/// slow host delays a row instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(180);

/// How long a poll sleeps between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The shortest usage a worker is billed for, as `JobRun$billedResourceUtilization` documents
/// it ("a 1-minute minimum usage for workers"), in hours.
const BILLED_MINIMUM_HOURS: f64 = 1.0 / 60.0;

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

/// One EMR Serverless call (restJson1), answered as `(status, body)`.
async fn emr_serverless(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let host = format!("emr-serverless.{REGION}.amazonaws.com");
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", host)
        .header("authorization", authorization("emr-serverless"));
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, answered)
}

/// The same, for a call that must be served.
async fn emr_serverless_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Value {
    let (status, answered) = emr_serverless(router, method, path, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {method} {path}, got {status} with {answered}"
    );
    answered
}

/// One path-style Amazon S3 request: its status and body.
async fn s3(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
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

/// The object at `key`, or `None` when the store holds none.
async fn object(router: &MultiServiceRouter, key: &str) -> Option<String> {
    let (status, body) = s3(router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
    (200..300)
        .contains(&status)
        .then(|| String::from_utf8_lossy(&body).to_string())
}

/// The bucket every row shares, created through the service that owns it.
async fn given_bucket(router: &MultiServiceRouter) {
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status) || status == 409,
        "creating s3://{BUCKET} must succeed or report it is already there, got {status} {}",
        String::from_utf8_lossy(&body)
    );
}

/// An application brought up to [`STARTED`], the way a client brings one up: create, poll,
/// start, poll.
async fn started_application(router: &MultiServiceRouter, token: &str) -> String {
    let created = emr_serverless_ok(
        router,
        "setup",
        "POST",
        "/applications",
        Some(json!({
            "clientToken": token,
            "name": "job-run-execution",
            "releaseLabel": RELEASE_LABEL,
            "type": ENGINE_TYPE,
        })),
    )
    .await;
    let application_id = created["applicationId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateApplication answers an applicationId, got {created}"))
        .to_string();
    poll_application(router, &application_id, &["CREATED", STARTED]).await;
    emr_serverless_ok(
        router,
        "setup",
        "POST",
        &format!("/applications/{application_id}/start"),
        None,
    )
    .await;
    poll_application(router, &application_id, &[STARTED]).await;
    application_id
}

/// Read one application until its state is one of `wanted`.
async fn poll_application(router: &MultiServiceRouter, application_id: &str, wanted: &[&str]) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let answered = emr_serverless_ok(
            router,
            "setup",
            "GET",
            &format!("/applications/{application_id}"),
            None,
        )
        .await;
        let state = answered["application"]["state"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if wanted.contains(&state.as_str()) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the application never reached {wanted:?}, last state {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Read one job run until it reaches a state it never leaves, and answer the `jobRun` as last
/// read.
async fn settled_job_run(
    router: &MultiServiceRouter,
    case: &str,
    application_id: &str,
    job_run_id: &str,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let answered = emr_serverless_ok(
            router,
            case,
            "GET",
            &format!("/applications/{application_id}/jobruns/{job_run_id}"),
            None,
        )
        .await;
        let run = answered["jobRun"].clone();
        let state = run["state"].as_str().unwrap_or_default().to_string();
        if TERMINAL_STATES.contains(&state.as_str()) {
            return run;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job run never settled, last state {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The sum of every value in `1..=100` whose remainder modulo three is `remainder`, computed
/// here so the expected output is this suite's own arithmetic rather than the emulator's.
fn modular_sum(remainder: u32) -> u32 {
    (1..=100u32).filter(|value| value % 3 == remainder).sum()
}

/// What a row's program comes to.
enum Ends {
    /// It exited cleanly: the run succeeds, and these `(key, content)` pairs are the objects
    /// the program really wrote.
    Succeeds(Vec<(String, String)>),
    /// It failed: the run's `stateDetails` carry every one of these fragments.
    Fails(Vec<String>),
}

/// One row: the program this job run submits, how the submit sizes its driver, and the end it
/// must reach.
struct Row {
    case: &'static str,
    source: String,
    /// The arguments the submit passes the program, after its own path.
    arguments: Vec<String>,
    /// The `sparkSubmitParameters` the submit carries, which is what sizes the driver the run
    /// is measured over.
    parameters: Option<&'static str>,
    ends: Ends,
}

fn rows() -> Vec<Row> {
    let expected_sums = format!(
        "0,{}\n1,{}\n2,{}\n",
        modular_sum(0),
        modular_sum(1),
        modular_sum(2)
    );
    vec![
        Row {
            case: "a keyed reduction written through the Hadoop filesystem",
            source: r#"
import sys
from pyspark.sql import SparkSession

spark = SparkSession.builder.getOrCreate()
rows = (
    spark.sparkContext.parallelize(range(1, 101), 2)
    .map(lambda value: (value % 3, value))
    .reduceByKey(lambda left, right: left + right)
    .sortByKey()
    .collect()
)
text = "".join("%d,%d\n" % row for row in rows)
path = spark._jvm.org.apache.hadoop.fs.Path(sys.argv[1])
out = path.getFileSystem(spark._jsc.hadoopConfiguration()).create(path, True)
out.write(bytearray(text.encode("utf-8")))
out.close()
spark.stop()
"#
            .to_string(),
            arguments: vec![format!("s3://{BUCKET}/out/sums.csv")],
            parameters: Some(
                "--conf spark.driver.cores=2 --conf spark.driver.memory=8g --conf \
                 spark.executor.instances=1",
            ),
            ends: Ends::Succeeds(vec![("out/sums.csv".to_string(), expected_sums)]),
        },
        Row {
            case: "a filtered dataset saved as text, one part file per partition",
            source: r#"
import sys
from pyspark import SparkContext

sc = SparkContext.getOrCreate()
(
    sc.parallelize(range(1, 11), 3)
    .filter(lambda value: value % 2 == 0)
    .map(lambda value: value * value)
    .saveAsTextFile(sys.argv[1])
)
sc.stop()
"#
            .to_string(),
            arguments: vec![format!("s3://{BUCKET}/out/squares")],
            parameters: None,
            ends: Ends::Succeeds(vec![
                ("out/squares/part-00000".to_string(), "4\n16\n".to_string()),
                ("out/squares/part-00001".to_string(), "36\n".to_string()),
                (
                    "out/squares/part-00002".to_string(),
                    "64\n100\n".to_string(),
                ),
                ("out/squares/_SUCCESS".to_string(), String::new()),
            ]),
        },
        Row {
            case: "a program that raises",
            source: r#"
raise ValueError("the submitted program failed")
"#
            .to_string(),
            arguments: Vec::new(),
            parameters: None,
            // The program's OWN exception, as the engine that ran it reported it.
            ends: Ends::Fails(vec![
                "ValueError".to_string(),
                "the submitted program failed".to_string(),
            ]),
        },
        Row {
            case: "a program that exits with a status of its own",
            source: r#"
import sys

sys.exit(3)
"#
            .to_string(),
            arguments: Vec::new(),
            parameters: None,
            // A non-zero exit is the program failing, and the status travels on as its own.
            ends: Ends::Fails(vec!["SystemExit".to_string(), "3".to_string()]),
        },
    ]
}

/// The key one row's program is put under.
fn program_key(ordinal: usize) -> String {
    format!("programs/program-{ordinal}.py")
}

/// A floating-point member of a utilisation structure, which must be present.
fn utilisation(case: &str, utilisation: &Value, member: &str) -> f64 {
    utilisation[member].as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: expected {member} to be reported, got {utilisation}")
    })
}

#[tokio::test]
async fn a_job_run_runs_its_submitted_program_and_reports_what_it_took() {
    let router = router();
    let rows = rows();
    given_bucket(&router).await;
    let application_id = started_application(&router, "tok-job-run-execution").await;

    // Every program is put first and every run submitted before any is polled, so the clock
    // walks them together rather than one after another.
    for (ordinal, row) in rows.iter().enumerate() {
        put_object(
            &router,
            row.case,
            &program_key(ordinal),
            row.source.as_bytes(),
        )
        .await;
    }
    let mut submitted: Vec<String> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        let mut driver = json!({
            "sparkSubmit": {
                "entryPoint": format!("s3://{BUCKET}/{}", program_key(ordinal)),
                "entryPointArguments": row.arguments.clone(),
            }
        });
        if let Some(parameters) = row.parameters {
            driver["sparkSubmit"]["sparkSubmitParameters"] = json!(parameters);
        }
        let answered = emr_serverless_ok(
            &router,
            row.case,
            "POST",
            &format!("/applications/{application_id}/jobruns"),
            Some(json!({
                "clientToken": format!("tok-run-{ordinal}"),
                "executionRoleArn": EXECUTION_ROLE,
                "jobDriver": driver,
                "tags": { "owner": "suite" },
                "retryPolicy": { "maxAttempts": 1 },
                "configurationOverrides": {
                    "monitoringConfiguration": {
                        "managedPersistenceMonitoringConfiguration": { "enabled": true }
                    }
                },
            })),
        )
        .await;
        submitted.push(
            answered["jobRunId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: StartJobRun answers a jobRunId, got {answered}",
                        row.case
                    )
                })
                .to_string(),
        );
    }

    for (row, job_run_id) in rows.iter().zip(&submitted) {
        let case = row.case;
        let run = settled_job_run(&router, case, &application_id, job_run_id).await;

        // Whatever the program came to, the run reports what the submit asked for and what
        // the run itself took.
        assert_eq!(
            run["mode"],
            json!("BATCH"),
            "case {case:?}: a run submitted with no mode is a BATCH run, got {run}"
        );
        assert_eq!(
            run["retryPolicy"]["maxAttempts"],
            json!(1),
            "case {case:?}: the submitted retry policy is reported back, got {run}"
        );
        assert_eq!(
            run["tags"],
            json!({ "owner": "suite" }),
            "case {case:?}: the submitted tags are reported back, got {run}"
        );
        assert_eq!(
            run["configurationOverrides"]["monitoringConfiguration"]
                ["managedPersistenceMonitoringConfiguration"]["enabled"],
            json!(true),
            "case {case:?}: the submitted configuration overrides are reported back, got {run}"
        );
        assert_eq!(
            run["extendedSupportEnabled"],
            json!(false),
            "case {case:?}: the run reports its extended-support flag, got {run}"
        );

        // It EXECUTED: it moved to RUNNING, it ended no earlier than it started, and the
        // duration it reports is that span.
        let started_at = run["startedAt"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: a run that executed reports startedAt, got {run}")
        });
        let ended_at = run["endedAt"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: a terminated run reports endedAt, got {run}")
        });
        assert!(
            started_at <= ended_at,
            "case {case:?}: expected startedAt <= endedAt, got {started_at} and {ended_at}"
        );
        let duration = run["totalExecutionDurationSeconds"]
            .as_f64()
            .unwrap_or_else(|| {
                panic!("case {case:?}: a terminated run reports its duration, got {run}")
            });
        let span = ended_at - started_at;
        assert!(
            duration >= span && duration <= span + 1.0,
            "case {case:?}: expected the duration to be the span it executed ({span}s) \
             rounded up, got {duration}"
        );
        let queued = run["queuedDurationMilliseconds"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: the run reports the time it spent QUEUED, got {run}")
        });
        assert!(
            queued >= 0.0,
            "case {case:?}: expected a queued duration, got {queued}"
        );

        // ... and what it consumed: the aggregate over the span it executed, and the billed
        // figures under the documented one-minute minimum and 20 GB of free storage.
        let total = run["totalResourceUtilization"].clone();
        let billed = run["billedResourceUtilization"].clone();
        for member in ["vCPUHour", "memoryGBHour", "storageGBHour"] {
            assert!(
                utilisation(case, &total, member) > 0.0,
                "case {case:?}: expected {member} to be what the run used, got {total}"
            );
        }
        let total_v_cpu = utilisation(case, &total, "vCPUHour");
        let billed_v_cpu = utilisation(case, &billed, "vCPUHour");
        assert!(
            billed_v_cpu >= total_v_cpu,
            "case {case:?}: the billed vCPU-hours cover at least the minimum usage, got \
             {billed_v_cpu} against {total_v_cpu}"
        );
        // Each row's driver holds the per-worker storage that is not billed, so nothing of
        // its storage is.
        assert_eq!(
            utilisation(case, &billed, "storageGBHour"),
            0.0,
            "case {case:?}: a worker at the free storage size is billed no storage, got \
             {billed}"
        );
        let v_cpu_per_hour = total_v_cpu / (duration / 3600.0);
        assert!(
            billed_v_cpu >= v_cpu_per_hour * BILLED_MINIMUM_HOURS - f64::EPSILON,
            "case {case:?}: expected at least the one-minute minimum billed, got {billed}"
        );

        match &row.ends {
            Ends::Succeeds(objects) => {
                assert_eq!(
                    run["state"],
                    json!("SUCCESS"),
                    "case {case:?}: a program that exited cleanly succeeds, got {run}"
                );
                assert_eq!(
                    run["stateDetails"],
                    json!(""),
                    "case {case:?}: a successful run carries no state details, got {run}"
                );
                // The objects the program wrote are objects the store really holds.
                for (key, expected) in objects {
                    let stored = object(&router, key).await.unwrap_or_else(|| {
                        panic!("case {case:?}: the store holds the object the program wrote \
                                at {key:?}")
                    });
                    assert_eq!(
                        &stored, expected,
                        "case {case:?}: expected {key:?} to hold {expected:?}, got {stored:?}"
                    );
                }
            }
            Ends::Fails(fragments) => {
                assert_eq!(
                    run["state"],
                    json!("FAILED"),
                    "case {case:?}: a program that failed fails its run, got {run}"
                );
                let details = run["stateDetails"].as_str().unwrap_or_default().to_string();
                for fragment in fragments {
                    assert!(
                        details.contains(fragment),
                        "case {case:?}: expected the details to carry {fragment:?}, got \
                         {details:?}"
                    );
                }
                assert!(
                    !details.contains("emulator"),
                    "case {case:?}: a run that really failed reports the program's own error, \
                     got {details:?}"
                );
            }
        }
    }
}

/// The MODE a run is in and the retry policy in force on it are reported by every read: the
/// submit's own values, or the default for the mode where the submit named none.
///
/// The rows differ along the mode (named and absent) and along which member of the policy the
/// submit set — `RetryPolicy$maxAttempts` "is only applicable for BATCH mode" and
/// `maxFailedAttemptsPerHour` "is only applicable for STREAMING mode", so each mode's own
/// member is the one that carries a value.
#[tokio::test]
async fn a_read_reports_the_mode_and_the_retry_policy_in_force() {
    // One row of the projection table: how the submit names the mode and the retry policy, and
    // what a read must then report.
    struct Projection {
        case: &'static str,
        mode: Option<&'static str>,
        policy: Option<Value>,
        expected_mode: &'static str,
        max_attempts: Option<i64>,
        per_hour: Option<i64>,
    }

    let cases = [
        Projection {
            case: "no mode and no policy",
            mode: None,
            policy: None,
            expected_mode: "BATCH",
            max_attempts: Some(1),
            per_hour: None,
        },
        Projection {
            case: "a batch run with a policy of its own",
            mode: Some("BATCH"),
            policy: Some(json!({ "maxAttempts": 4 })),
            expected_mode: "BATCH",
            max_attempts: Some(4),
            per_hour: None,
        },
        Projection {
            case: "a streaming run with no policy",
            mode: Some("STREAMING"),
            policy: None,
            expected_mode: "STREAMING",
            max_attempts: None,
            per_hour: Some(5),
        },
        Projection {
            case: "a streaming run with a policy of its own",
            mode: Some("STREAMING"),
            policy: Some(json!({ "maxFailedAttemptsPerHour": 2 })),
            expected_mode: "STREAMING",
            max_attempts: None,
            per_hour: Some(2),
        },
    ];

    let router = router();
    given_bucket(&router).await;
    let application_id = started_application(&router, "tok-job-run-projection").await;
    // One program, submitted by every row: what is under test is the projection of the
    // submit, which is set when the run is accepted.
    let key = "programs/noop.py";
    put_object(&router, "setup", key, b"pass\n").await;

    for (index, row) in cases.into_iter().enumerate() {
        let case = row.case;
        let mut request = json!({
            "clientToken": format!("tok-projection-{index}"),
            "executionRoleArn": EXECUTION_ROLE,
            "jobDriver": {
                "sparkSubmit": { "entryPoint": format!("s3://{BUCKET}/{key}") }
            },
        });
        if let Some(mode) = row.mode {
            request["mode"] = json!(mode);
        }
        if let Some(policy) = row.policy {
            request["retryPolicy"] = policy;
        }
        let answered = emr_serverless_ok(
            &router,
            case,
            "POST",
            &format!("/applications/{application_id}/jobruns"),
            Some(request),
        )
        .await;
        let job_run_id = answered["jobRunId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: StartJobRun answers a jobRunId"))
            .to_string();
        let read = emr_serverless_ok(
            &router,
            case,
            "GET",
            &format!("/applications/{application_id}/jobruns/{job_run_id}"),
            None,
        )
        .await["jobRun"]
            .clone();

        assert_eq!(
            read["mode"],
            json!(row.expected_mode),
            "case {case:?}: expected mode {:?}, got {read}",
            row.expected_mode
        );
        assert_eq!(
            read["retryPolicy"]["maxAttempts"].as_i64(),
            row.max_attempts,
            "case {case:?}: expected maxAttempts {:?}, got {read}",
            row.max_attempts
        );
        assert_eq!(
            read["retryPolicy"]["maxFailedAttemptsPerHour"].as_i64(),
            row.per_hour,
            "case {case:?}: expected maxFailedAttemptsPerHour {:?}, got {read}",
            row.per_hour
        );
        // A run submitted with no tags carries the EMPTY tag map, not no tag map, and the
        // monitoring configuration in force is reported whether the submit named one or not.
        assert_eq!(
            read["tags"],
            json!({}),
            "case {case:?}: expected the empty tag map, got {read}"
        );
        assert_eq!(
            read["configurationOverrides"]["monitoringConfiguration"]
                ["managedPersistenceMonitoringConfiguration"]["enabled"],
            json!(true),
            "case {case:?}: expected the monitoring configuration in force, got {read}"
        );
    }
}
