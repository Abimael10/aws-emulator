// SPDX-License-Identifier: CC0-1.0

//! What an EMR Serverless job run REPORTS about the execution it performed, observed from the
//! submit until the run comes to rest.
//!
//! THE RULE: a job run is "a unit of work, such as a Spark JAR, Hive query, or SparkSQL query,
//! that you submit to an Amazon EMR Serverless application" (`JobRun`), so the state it comes
//! to rest in is what running that unit of work came to — `SUCCESS` for work that ran to its
//! end — and the span it reports is the span it really executed for: `JobRun$startedAt` is "The
//! date and time when the job moved to the RUNNING state", `endedAt` "The date and time when
//! the job was terminated" and `totalExecutionDurationSeconds` "The job run total execution
//! duration in seconds. This field is only available for job runs in a COMPLETED, FAILED, or
//! CANCELLED state". A run therefore reports NEITHER the instant it ended NOR the duration it
//! took while it is still in a non-terminal `JobRunState`, reports `startedAt` from the moment
//! it is `RUNNING`, and reports all three once it rests — the duration being the span between
//! the two instants. Everything it carries is about the RUN: a `stateDetails` describes the
//! state the run is in or the outcome its own work reached, never the service serving it. The
//! objects the work wrote are then real objects of Amazon S3, so the part files Spark's writer
//! produced are the ones `ListObjectsV2` reports under the output prefix the submit named.
//!
//! Only a cross-service suite can pin this: the program that runs is an object a client put in
//! Amazon S3, the execution role is a role IAM holds, and the output is discovered and read
//! back through Amazon S3's own listing and reads — never through anything this suite handed
//! EMR Serverless. It therefore runs against the WHOLE `build_router` service set.
//!
//! The rows differ along what the rule generalises over: what the submitted program comes to
//! (it writes its output and exits cleanly, it does its work and then raises), how many
//! partitions its work is sliced into (one, three), how deep the output prefix it writes under
//! is, how many arguments the submit passes it, and whether the submit sizes the driver with
//! `sparkSubmitParameters`.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for: the one whose `CreateBucket` needs no
/// location constraint in its body.
const REGION: &str = "us-east-1";

/// The bucket every program of this suite is read from and every output is written to.
const BUCKET: &str = "emr-serverless-execution-span";

/// The name of the execution role every submit names — "The execution role ARN for the job
/// run" (`StartJobRunRequest$executionRoleArn`).
const EXECUTION_ROLE: &str = "EMRServerlessExecutionSpanRole";

/// The service principal EMR Serverless assumes a job run's execution role as.
const EMR_SERVERLESS_PRINCIPAL: &str = "emr-serverless.amazonaws.com";

/// The Amazon EMR release the application of this suite runs on, and the engine it serves.
const RELEASE_LABEL: &str = "emr-6.15.0";
const ENGINE_TYPE: &str = "SPARK";

/// The NON-TERMINAL `JobRunState` values a submitted run moves through, in that order. Source:
/// the `JobRunState` enum (`smithy_query(query="shape",shape="JobRunState")`): `SUBMITTED |
/// PENDING | SCHEDULED | RUNNING | SUCCESS | FAILED | CANCELLING | CANCELLED | QUEUED`, whose
/// `CANCELLING` belongs to a run a caller asked to cancel and so is reached by none of the rows
/// below. Written out here rather than imported from the service crate, so a wrong constant in
/// `src/` cannot make this suite pass.
const LIFECYCLE_ORDER: [&str; 5] = ["SUBMITTED", "PENDING", "QUEUED", "SCHEDULED", "RUNNING"];

/// The `JobRunState` values a run never leaves — the three of that enum that name a finished
/// run, which is also the set `totalExecutionDurationSeconds` is documented as available in.
const TERMINAL_STATES: [&str; 3] = ["SUCCESS", "FAILED", "CANCELLED"];

/// The `JobRunState` a run carries while its own work executes.
const RUNNING: &str = "RUNNING";

/// How long a poll waits for the condition it waits on before declaring the work never
/// happened.
///
/// Generous rather than tight: what it waits for is the stages the clock owes a submitted run
/// plus one execution of a submitted program, and a host running the whole workspace's tests
/// at once gives that execution a small share of the machine. Nothing here asserts how long
/// anything took, so this bound only has to be far enough above the work to be no part of what
/// is observed. It matches what the sibling job-run suites of this directory wait.
const DEADLINE: Duration = Duration::from_secs(180);

/// How often a poll looks again. Short enough that a client polling at this interval sees the
/// run in the states it passes through, which is the half of the rule that needs observing
/// WHILE the run is in flight.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The slack the comparison of a reported duration against the instants it is derived from
/// allows: the two instants are read off the WIRE, where a timestamp carries fewer digits than
/// the instant it was computed from, so a span either side of a whole second may be read as the
/// other one. A millisecond covers that and nothing else.
const WIRE_PRECISION: f64 = 0.001;

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

/// The text one object holds, read with `GetObject`.
async fn object_text(router: &MultiServiceRouter, case: &str, key: &str) -> String {
    let (status, body) = s3(router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: GetObject {key} must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    String::from_utf8_lossy(&body).into_owned()
}

/// The keys `ListObjectsV2` reports under `prefix`, which is how a client finds what a run's
/// own writer produced.
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
        "case {case:?}: ListObjectsV2 on {prefix:?} must succeed, got {status} with {listing}"
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
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
    .replace('"', "%22")
    .replace(' ', "%20")
}

/// The identity policy the execution role carries: the object-store actions a job run's own
/// reads and writes perform, on this suite's bucket and nothing else.
fn execution_policy() -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Action\":[\"s3:GetObject\",\"s3:PutObject\",\"s3:ListBucket\"],\
         \"Resource\":[\"arn:aws:s3:::{BUCKET}\",\"arn:aws:s3:::{BUCKET}/*\"]}}]}}"
    )
    .replace('"', "%22")
    .replace(' ', "%20")
    .replace('[', "%5B")
    .replace(']', "%5D")
}

/// The ARN of the role above, as a submit names it.
fn execution_role_arn() -> String {
    format!("arn:aws:iam::000000000000:role/{EXECUTION_ROLE}")
}

/// The bucket and the execution role every row shares, each created through the service that
/// owns it.
async fn seed(router: &MultiServiceRouter) {
    let case = "setup";
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status) || status == 409,
        "case {case:?}: CreateBucket must succeed or report the bucket is there, got {status} \
         {}",
        String::from_utf8_lossy(&body)
    );
    iam(
        router,
        case,
        &format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={EXECUTION_ROLE}\
             &AssumeRolePolicyDocument={}",
            trust_policy(EMR_SERVERLESS_PRINCIPAL)
        ),
    )
    .await;
    iam(
        router,
        case,
        &format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName={EXECUTION_ROLE}\
             &PolicyName=execution-span&PolicyDocument={}",
            execution_policy()
        ),
    )
    .await;
}

/// An application brought up to `STARTED`, which is what a job run is submitted to: created
/// with the capacity ceiling and the auto-start/auto-stop configuration a client gives one, and
/// then polled until it holds capacity.
async fn started_application(router: &MultiServiceRouter, token: &str) -> String {
    let case = "setup";
    let created = emr_serverless_ok(
        router,
        case,
        "POST",
        "/applications",
        &json!({
            "clientToken": token,
            "name": "execution-span",
            "releaseLabel": RELEASE_LABEL,
            "type": ENGINE_TYPE,
            "maximumCapacity": { "cpu": "8 vCPU", "memory": "32 GB" },
            "autoStartConfiguration": { "enabled": true },
            "autoStopConfiguration": { "enabled": true, "idleTimeoutMinutes": 10 },
        }),
    )
    .await;
    let application_id = created["applicationId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: CreateApplication must answer an applicationId, got {created}")
        })
        .to_string();
    emr_serverless_ok(
        router,
        case,
        "POST",
        &format!("/applications/{application_id}/start"),
        &json!({}),
    )
    .await;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let answered = emr_serverless_ok(
            router,
            case,
            "GET",
            &format!("/applications/{application_id}"),
            &json!({}),
        )
        .await;
        let state = answered["application"]["state"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if state == "STARTED" {
            return application_id;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the application must reach STARTED, last state {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `GetJobRun`, as the `JobRun` it answers.
async fn job_run(
    router: &MultiServiceRouter,
    case: &str,
    application_id: &str,
    job_run_id: &str,
) -> Value {
    emr_serverless_ok(
        router,
        case,
        "GET",
        &format!("/applications/{application_id}/jobruns/{job_run_id}"),
        &json!({}),
    )
    .await["jobRun"]
        .clone()
}

/// A program that writes the text it was given, in ONE partition, the way a Spark driver
/// writing its result does.
const WRITES_ONE_PARTITION: &str = r#"
import sys

from pyspark.sql import SparkSession

spark = SparkSession.builder.appName("one-partition").getOrCreate()
spark.sparkContext.parallelize([sys.argv[1]], 1).saveAsTextFile(sys.argv[2])
spark.stop()
"#;

/// A program whose work is sliced into THREE partitions, so its writer leaves one part file per
/// partition.
const WRITES_THREE_PARTITIONS: &str = r#"
import sys

from pyspark import SparkContext

sc = SparkContext.getOrCreate()
count = int(sys.argv[1])
rows = ["%s-%d" % (sys.argv[2], index) for index in range(count)]
sc.parallelize(rows, 3).saveAsTextFile(sys.argv[3])
sc.stop()
"#;

/// A program that does its work and then RAISES, writing nothing: the shape of a driver that
/// fails after building its session.
const RAISES_WITHOUT_WRITING: &str = r#"
import sys

from pyspark.sql import SparkSession

spark = SparkSession.builder.appName("raises").getOrCreate()
total = spark.sparkContext.parallelize(range(1, 5), 1).sum()
raise RuntimeError("the driver gave up after %d" % int(total))
"#;

/// What a row's program comes to.
enum Ends {
    /// It exited cleanly: the run reaches `SUCCESS`, and `ListObjectsV2` reports exactly this
    /// many part files under its output prefix, holding these lines between them.
    Succeeds {
        parts: usize,
        lines: BTreeSet<String>,
    },
    /// It failed: the run reaches `FAILED` carrying every one of these fragments of its own
    /// error, and its output prefix holds nothing at all.
    Fails(Vec<&'static str>),
}

/// One row: the program this run submits, how the submit sizes its driver, and the end it must
/// reach.
struct Row {
    case: &'static str,
    source: &'static str,
    /// The arguments the submit passes the program, after its own path.
    arguments: Vec<String>,
    /// The prefix the program's output is written under, which is where this row looks for it.
    output_prefix: String,
    /// `SparkSubmit$sparkSubmitParameters` the submit carries, if any.
    parameters: Option<&'static str>,
    ends: Ends,
}

/// The text this suite asks the one-partition program to write.
const WRITTEN_TEXT: &str = "execution-span-marker";

/// The prefix the three-partition program labels its rows with.
const ROW_LABEL: &str = "row";

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a program that writes its argument in one partition and exits cleanly",
            source: WRITES_ONE_PARTITION,
            arguments: vec![
                WRITTEN_TEXT.to_string(),
                format!("s3://{BUCKET}/out/one"),
            ],
            output_prefix: "out/one".to_string(),
            parameters: None,
            ends: Ends::Succeeds {
                parts: 1,
                lines: [WRITTEN_TEXT.to_string()].into_iter().collect(),
            },
        },
        Row {
            case: "a program whose work is sliced into three partitions, with the driver sized \
                   by the submit",
            source: WRITES_THREE_PARTITIONS,
            arguments: vec![
                "6".to_string(),
                ROW_LABEL.to_string(),
                format!("s3://{BUCKET}/out/nested/three/partitions"),
            ],
            output_prefix: "out/nested/three/partitions".to_string(),
            parameters: Some(
                "--conf spark.executor.cores=1 --conf spark.executor.memory=2g \
                 --conf spark.driver.cores=1 --conf spark.driver.memory=2g \
                 --conf spark.executor.instances=1",
            ),
            ends: Ends::Succeeds {
                parts: 3,
                lines: (0..6).map(|index| format!("{ROW_LABEL}-{index}")).collect(),
            },
        },
        Row {
            case: "a program that does its work and then raises, writing nothing",
            source: RAISES_WITHOUT_WRITING,
            arguments: Vec::new(),
            output_prefix: "out/raised".to_string(),
            parameters: None,
            // The program's OWN exception, as the engine that ran it reported it: the sum its
            // own arithmetic produced is in the message it raised.
            ends: Ends::Fails(vec!["RuntimeError", "the driver gave up after 10"]),
        },
    ]
}

/// One reading of a run: the state it was in and everything it reported about its execution at
/// that moment.
struct Observation {
    state: String,
    details: String,
    /// Whether `startedAt`, `endedAt` and `totalExecutionDurationSeconds` were reported.
    started_at: Option<f64>,
    ended_at: Option<f64>,
    duration: Option<i64>,
}

impl Observation {
    fn of(run: &Value) -> Self {
        Self {
            state: run["state"].as_str().unwrap_or_default().to_string(),
            details: run["stateDetails"].as_str().unwrap_or_default().to_string(),
            started_at: run["startedAt"].as_f64(),
            ended_at: run["endedAt"].as_f64(),
            duration: run["totalExecutionDurationSeconds"].as_i64(),
        }
    }

    fn is_terminal(&self) -> bool {
        TERMINAL_STATES.contains(&self.state.as_str())
    }
}

/// Where `state` sits in the order a submitted run moves through the modelled states: every
/// terminal state is past the last non-terminal one, and a state of neither kind has no place
/// in the lifecycle at all.
fn lifecycle_position(state: &str) -> Option<usize> {
    if TERMINAL_STATES.contains(&state) {
        return Some(LIFECYCLE_ORDER.len());
    }
    LIFECYCLE_ORDER.iter().position(|held| *held == state)
}

/// THE RULE: a run reports the span it really executed for — nothing of it while it is still
/// in flight, `startedAt` from the moment it is `RUNNING`, and the instants plus the duration
/// between them once it rests on what running its work came to — and the objects that work
/// wrote are the ones Amazon S3's listing reports under the output prefix it was given.
#[tokio::test]
async fn a_job_run_reports_the_span_its_execution_took_and_what_it_wrote() {
    let router = router();
    let rows = rows();
    seed(&router).await;
    let application_id = started_application(&router, "execution-span-token").await;

    // Every row's program is staged and every run submitted before any is polled, so the runs
    // are in flight together rather than one after another.
    let mut submitted: Vec<(String, String)> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        let key = format!("programs/{ordinal}/main.py");
        put_object(&router, row.case, &key, row.source.as_bytes()).await;
        let entry_point = format!("s3://{BUCKET}/{key}");
        let mut spark_submit = json!({
            "entryPoint": entry_point,
            "entryPointArguments": row.arguments,
        });
        if let Some(parameters) = row.parameters {
            spark_submit["sparkSubmitParameters"] = json!(parameters);
        }
        let answered = emr_serverless_ok(
            &router,
            row.case,
            "POST",
            &format!("/applications/{application_id}/jobruns"),
            &json!({
                "clientToken": format!("execution-span-{ordinal}"),
                "executionRoleArn": execution_role_arn(),
                "jobDriver": json!({ "sparkSubmit": spark_submit }),
            }),
        )
        .await;
        let job_run_id = answered["jobRunId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: StartJobRun must answer a jobRunId, got {answered}",
                    row.case
                )
            })
            .to_string();
        submitted.push((job_run_id, entry_point));
    }

    // Every state each run is observed in, from the submit until it comes to rest: the look is
    // a POLL for the next change rather than a wait of any length.
    let mut observed: Vec<Vec<Observation>> = rows.iter().map(|_| Vec::new()).collect();
    let mut resting: Vec<Option<Value>> = rows.iter().map(|_| None).collect();
    let deadline = Instant::now() + DEADLINE;
    loop {
        let mut in_flight: Vec<String> = Vec::new();
        for (index, (job_run_id, _entry_point)) in submitted.iter().enumerate() {
            if resting[index].is_some() {
                continue;
            }
            let case = rows[index].case;
            let run = job_run(&router, case, &application_id, job_run_id).await;
            let observation = Observation::of(&run);
            if observed[index]
                .last()
                .is_none_or(|last: &Observation| last.state != observation.state)
            {
                let terminal = observation.is_terminal();
                observed[index].push(observation);
                if terminal {
                    resting[index] = Some(run);
                    continue;
                }
            }
            in_flight.push(format!(
                "{case:?} is {:?}",
                observed[index]
                    .last()
                    .map(|last| last.state.clone())
                    .unwrap_or_default()
            ));
        }
        if in_flight.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "these job runs never came to rest: {in_flight:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let readings = &observed[index];
        let states: Vec<&str> = readings.iter().map(|held| held.state.as_str()).collect();

        // The states a submitted run is observed in are modelled ones, in the order the
        // lifecycle takes them, beginning where the submit left the run.
        assert_eq!(
            states.first(),
            Some(&"SUBMITTED"),
            "case {case:?}: a run is SUBMITTED when it is accepted, got {states:?}"
        );
        let mut reached = 0;
        for state in &states {
            let position = lifecycle_position(state).unwrap_or_else(|| {
                panic!("case {case:?}: {state:?} is not a modelled JobRunState, got {states:?}")
            });
            assert!(
                position >= reached,
                "case {case:?}: the lifecycle never moves backwards, got {states:?}"
            );
            reached = position;
        }
        // A run is RUNNING for as long as its own work takes, and the stage before it is owed
        // by the clock, so a client looking again every POLL_INTERVAL sees the run in that
        // state: the window is the service's and nothing here waits a length of time for it.
        assert!(
            states.contains(&RUNNING),
            "case {case:?}: a run executing its submitted program must be observed RUNNING, \
             got {states:?}"
        );

        // While the run is still in flight it reports NOTHING of the span it has not finished:
        // no instant it was terminated at and no total duration. `startedAt` appears exactly
        // when the run moves to RUNNING, which is the instant the member documents.
        for reading in readings.iter().filter(|held| !held.is_terminal()) {
            assert!(
                reading.ended_at.is_none() && reading.duration.is_none(),
                "case {case:?}: a run in {:?} reports neither endedAt nor \
                 totalExecutionDurationSeconds, got {:?} and {:?}",
                reading.state,
                reading.ended_at,
                reading.duration
            );
            assert_eq!(
                reading.started_at.is_some(),
                reading.state == RUNNING,
                "case {case:?}: startedAt is reported from RUNNING onwards, and the run in \
                 {:?} reported {:?}",
                reading.state,
                reading.started_at
            );
            assert!(
                !reading.details.is_empty(),
                "case {case:?}: the state details of a run in {:?} are String256, so they say \
                 something, got {:?}",
                reading.state,
                reading.details
            );
        }

        // Nothing a run carries describes the service serving it: a stateDetails is about the
        // state the run is in or the outcome its own work reached.
        for reading in readings {
            assert!(
                !reading.details.to_ascii_lowercase().contains("emulator"),
                "case {case:?}: the state details of a run in {:?} describe the run, not what \
                 serves it, got {:?}",
                reading.state,
                reading.details
            );
        }

        // ... and once it rests, it reports the whole span it executed for.
        let rested = resting[index]
            .as_ref()
            .unwrap_or_else(|| panic!("case {case:?}: the run came to rest, got {states:?}"));
        let last = readings
            .last()
            .unwrap_or_else(|| panic!("case {case:?}: the run was observed at least once"));
        let started_at = last
            .started_at
            .unwrap_or_else(|| panic!("case {case:?}: a run that executed reports startedAt"));
        let ended_at = last
            .ended_at
            .unwrap_or_else(|| panic!("case {case:?}: a run that terminated reports endedAt"));
        assert!(
            started_at <= ended_at,
            "case {case:?}: expected startedAt {started_at} at or before endedAt {ended_at}"
        );
        let duration = last.duration.unwrap_or_else(|| {
            panic!(
                "case {case:?}: a run in {:?} reports totalExecutionDurationSeconds, got {rested}",
                last.state
            )
        });
        // The duration is the span between the two instants, rounded up, and a run that
        // executed at all occupied at least the one second it reports — so it is at least that
        // span and less than a second beyond it.
        let span = ended_at - started_at;
        let reported = duration as f64;
        assert!(
            duration >= 1
                && reported >= span - WIRE_PRECISION
                && reported <= span.max(1.0) + 1.0 + WIRE_PRECISION,
            "case {case:?}: the duration reported is the span between startedAt and endedAt, \
             expected the whole seconds of {span}, got {duration}"
        );

        // The run echoes the work it was asked to perform: the role it executed as and the
        // Spark submit it was submitted with.
        assert_eq!(
            rested["executionRole"].as_str(),
            Some(execution_role_arn().as_str()),
            "case {case:?}: the run reports the execution role it was submitted with, got {:?}",
            rested["executionRole"]
        );
        let submit = &rested["jobDriver"]["sparkSubmit"];
        assert_eq!(
            submit["entryPoint"].as_str(),
            Some(submitted[index].1.as_str()),
            "case {case:?}: the run reports the entry point it was submitted with, got {:?}",
            submit["entryPoint"]
        );
        let echoed: Vec<String> = submit["entryPointArguments"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .map(|value| value.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            echoed, row.arguments,
            "case {case:?}: the run reports the arguments its entry point was given, got \
             {echoed:?}"
        );
        assert_eq!(
            submit["sparkSubmitParameters"].as_str(),
            row.parameters,
            "case {case:?}: the run reports the submit parameters it carried, got {:?}",
            submit["sparkSubmitParameters"]
        );

        match &row.ends {
            Ends::Succeeds { parts, lines } => {
                assert_eq!(
                    last.state, "SUCCESS",
                    "case {case:?}: a program that ran to its end leaves the run in SUCCESS, \
                     got {:?} with state details {:?}",
                    last.state, last.details
                );
                // What the program WROTE, found the way a client finds it: the part files its
                // own writer produced, one per partition, under the prefix the submit named.
                let part_prefix = format!("{}/part-", row.output_prefix);
                let keys = keys_under(&router, case, &part_prefix).await;
                assert_eq!(
                    keys.len(),
                    *parts,
                    "case {case:?}: expected {parts} part file(s) under {part_prefix:?}, got \
                     {keys:?}"
                );
                assert!(
                    keys.iter().all(|key| key.starts_with(&part_prefix)),
                    "case {case:?}: a listing on {part_prefix:?} reports only the keys under \
                     it, got {keys:?}"
                );
                let mut held: BTreeSet<String> = BTreeSet::new();
                for key in &keys {
                    for line in object_text(&router, case, key).await.lines() {
                        held.insert(line.to_string());
                    }
                }
                assert_eq!(
                    &held, lines,
                    "case {case:?}: the part files hold the lines the program wrote, got {held:?}"
                );
                // The marker a completed write leaves is beside them, not among them: the
                // listing above answered the prefix it was asked for.
                let everything =
                    keys_under(&router, case, &format!("{}/", row.output_prefix)).await;
                assert!(
                    everything.iter().any(|key| key.ends_with("/_SUCCESS")),
                    "case {case:?}: a completed write leaves its marker beside its parts, got \
                     {everything:?}"
                );
            }
            Ends::Fails(fragments) => {
                assert_eq!(
                    last.state, "FAILED",
                    "case {case:?}: a program that raised fails its run, got {:?}",
                    last.state
                );
                for fragment in fragments {
                    assert!(
                        last.details.contains(fragment),
                        "case {case:?}: the state details carry the program's own \
                         {fragment:?}, got {:?}",
                        last.details
                    );
                }
                let keys = keys_under(&router, case, &format!("{}/", row.output_prefix)).await;
                assert!(
                    keys.is_empty(),
                    "case {case:?}: a program that wrote nothing leaves nothing under its \
                     output prefix, got {keys:?}"
                );
            }
        }
    }
}
