// SPDX-License-Identifier: CC0-1.0

//! An EMR Serverless job run comes to rest ON THE CLOCK, as soon as the work it submitted
//! answered — not on the next call that happens to read it.
//!
//! THE RULE: "A job run is a unit of work … that you submit to an Amazon EMR Serverless
//! application" (`JobRun` doc), `JobRun$startedAt` is "the date and time when the job moved to
//! the RUNNING state" and `endedAt` "when the job was terminated". So the terminal `JobRunState`
//! a submitted run reports is what RUNNING that unit of work came to, and it is owed to the run
//! by the clock from the instant that execution ANSWERED: a run reaches it whether or not
//! anybody is polling, and the first read taken after the owed span has passed reports the run
//! at rest. A reporting call is not what performs the work's outcome, so how often — and how
//! late — a client looks changes neither which state the run comes to rest in nor when.
//!
//! What each row therefore does is wait, WITHOUT CALLING THE SERVICE AT ALL, for the terminal
//! state to be owed to the run in the store, and only then take its ONE read of `GetJobRun`:
//! a run whose outcome is only recorded by a reader cannot pass that, because nothing has read
//! it. The span the clock then owes is made due THROUGH THE STORE rather than slept out, so
//! what these rows observe does not depend on how fast the host runs.
//!
//! The rows differ along what the rule generalises over: what the submitted program comes to
//! (it writes its output and exits cleanly, it does its work and then raises, it exits with a
//! status of its own), how many partitions its work is sliced into, and whether its writes
//! leave objects behind. Every program is an object a client PUT in Amazon S3 and every output
//! is read back with `GetObject`, so only a cross-service suite can pin it.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_emrserverless_awstoledowebservice::state::service_state;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for: the one whose `CreateBucket` needs no
/// location constraint in its body.
const REGION: &str = "us-east-1";

/// The bucket every program of this suite is read from and every output is written to.
const BUCKET: &str = "emr-serverless-settled-runs";

/// The execution role a job run is submitted with, satisfying `IAMRoleArn`'s pattern.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/EMRServerlessSettledRunRole";

/// The release the application runs on, and the engine it serves.
const RELEASE_LABEL: &str = "emr-7.1.0";
const ENGINE_TYPE: &str = "SPARK";

/// The `JobRunState` values a run never leaves, as the model's own `JobRunState` enum lists
/// them (`smithy_query(query="shape",shape="JobRunState")`) — written out here rather than
/// imported from the service crate, so a wrong constant in the implementation cannot make this
/// suite pass.
const TERMINAL_STATES: [&str; 3] = ["SUCCESS", "FAILED", "CANCELLED"];

/// The `ApplicationState` values this suite's bring-up waits for.
const CREATED: &str = "CREATED";
const STARTED: &str = "STARTED";

/// How long a poll waits for the condition it waits on before declaring that the work never
/// happened.
///
/// Generous rather than tight: what it waits for is one execution of a submitted program, which
/// is work of this emulator's own and not a span anybody waits out, and a host running a whole
/// suite at once must not be the thing that trips it. Nothing here asserts how long anything
/// took.
const PATIENCE: Duration = Duration::from_secs(180);

/// How often a poll asks again.
const POLL: Duration = Duration::from_millis(25);

/// EMR Serverless and Amazon S3 on one engine, plus the engine itself — so a row can read and
/// move the due instants a run's lifecycle turns on instead of waiting for them.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_emrserverless_awstoledowebservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    (router, engine)
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature itself is never checked: the router above runs with authentication
/// off.
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
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("emr-serverless.{REGION}.amazonaws.com"))
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

/// The bucket every row shares, created through the service that owns it.
async fn given_bucket(router: &MultiServiceRouter) {
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status) || status == 409,
        "creating s3://{BUCKET} must succeed or report it is already there, got {status} {}",
        String::from_utf8_lossy(&body)
    );
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
            "name": "settled-job-runs",
            "releaseLabel": RELEASE_LABEL,
            "type": ENGINE_TYPE,
        })),
    )
    .await;
    let application_id = created["applicationId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateApplication answers an applicationId, got {created}"))
        .to_string();
    poll_application(router, &application_id, &[CREATED, STARTED]).await;
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
    let deadline = Instant::now() + PATIENCE;
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
        tokio::time::sleep(POLL).await;
    }
}

/// Wait — reading the STORE and calling the service for nothing at all — until the terminal
/// state the run's own execution decided is OWED to it, and answer that state.
///
/// This is the half of the rule a reader cannot stand in for: the execution's outcome is
/// recorded on the run as that execution answers, so it is owed to the run before anybody has
/// asked about it. A run whose outcome only a reporting call would record never satisfies this,
/// because this poll makes no such call.
async fn terminal_state_owed(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    application_id: &str,
    job_run_id: &str,
) -> String {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let owed = {
            let state = service_state(&**engine).expect("the engine carries the EMR Serverless store");
            let store = state.lock();
            store
                .applications
                .get(application_id)
                .and_then(|application| application.job_runs.get(job_run_id))
                .and_then(|run| {
                    if TERMINAL_STATES.contains(&run.job_run.state.as_str()) {
                        return Some(run.job_run.state.clone());
                    }
                    run.pending
                        .iter()
                        .find(|pending| TERMINAL_STATES.contains(&pending.state.as_str()))
                        .map(|pending| pending.state.clone())
                })
        };
        if let Some(owed) = owed {
            return owed;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the run's execution must leave a terminal state owed to it without \
             anything reading the run, and run {job_run_id:?} has none"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Make every state the clock still owes this run DUE, by moving its due instants into the
/// past through the store — the span itself is not what any row is about, so it is moved
/// rather than slept out.
fn make_owed_states_due(
    engine: &Arc<engine::InMemoryEngine>,
    application_id: &str,
    job_run_id: &str,
) {
    let already_past = SystemTime::now() - Duration::from_secs(1);
    let state = service_state(&**engine).expect("the engine carries the EMR Serverless store");
    let mut store = state.lock();
    let Some(run) = store
        .applications
        .get_mut(application_id)
        .and_then(|application| application.job_runs.get_mut(job_run_id))
    else {
        panic!("the run {job_run_id:?} of {application_id:?} is held in the store");
    };
    for pending in &mut run.pending {
        pending.due = already_past;
    }
}

/// What a row's program comes to.
enum Ends {
    /// It exited cleanly: the run succeeds, and these `(key, content)` pairs are the objects the
    /// program really wrote.
    Succeeds(Vec<(&'static str, String)>),
    /// It failed: the run's `stateDetails` carry every one of these fragments, and these keys
    /// are the only objects it left behind (its output prefix holds nothing).
    Fails(Vec<&'static str>, Vec<&'static str>),
}

/// One row: the program this job run submits, and the end it must reach.
struct Row {
    case: &'static str,
    /// The key the program is PUT at, so each row's program is its own object.
    key: &'static str,
    /// The idempotency token the submit carries, inside `ClientToken`'s modelled
    /// `^[A-Za-z0-9._-]+$`.
    token: &'static str,
    source: &'static str,
    /// The arguments the submit passes the program, after its own path.
    arguments: Vec<String>,
    /// Keys no row's program may leave behind — the output prefix of a run that failed.
    absent: Vec<&'static str>,
    ends: Ends,
}

/// A program that writes the text it was given, in one partition.
const WRITES_ITS_ARGUMENT: &str = r#"
import sys
from pyspark.sql import SparkSession

spark = SparkSession.builder.getOrCreate()
spark.sparkContext.parallelize([sys.argv[1]], 1).saveAsTextFile(sys.argv[2])
spark.stop()
"#;

/// The same work sliced into two partitions, so the objects it leaves are the ones its own
/// partitioning produced.
const WRITES_TWO_PARTITIONS: &str = r#"
import sys
from pyspark import SparkContext

sc = SparkContext.getOrCreate()
sc.parallelize([value * 3 for value in range(1, 5)], 2).saveAsTextFile(sys.argv[1])
sc.stop()
"#;

/// A program that does its work, leaves a marker of having done it, and then raises — the
/// shape of a driver that fails after building its session.
const RAISES_AFTER_ITS_WORK: &str = r#"
import sys
from pyspark.sql import SparkSession

spark = SparkSession.builder.getOrCreate()
total = spark.sparkContext.parallelize(range(1, 5), 1).sum()
spark.sparkContext.parallelize([str(total)], 1).saveAsTextFile(sys.argv[1])
raise RuntimeError("the driver gave up after %d" % total)
"#;

/// A program that ends by exiting with a status of its own.
const EXITS_NON_ZERO: &str = r#"
import sys
from pyspark import SparkContext

SparkContext.getOrCreate().parallelize([1, 2, 3], 1).sum()
sys.exit(9)
"#;

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a program that writes its argument and exits cleanly",
            key: "job/writes_argument.py",
            token: "settled-writes-argument",
            source: WRITES_ITS_ARGUMENT,
            arguments: vec![
                "settled-marker".to_string(),
                format!("s3://{BUCKET}/out/argument"),
            ],
            absent: Vec::new(),
            ends: Ends::Succeeds(vec![
                ("out/argument/part-00000", "settled-marker\n".to_string()),
                ("out/argument/_SUCCESS", String::new()),
            ]),
        },
        Row {
            case: "a program whose work is sliced into two partitions",
            key: "job/writes_two_partitions.py",
            token: "settled-two-partitions",
            source: WRITES_TWO_PARTITIONS,
            arguments: vec![format!("s3://{BUCKET}/out/partitions")],
            absent: Vec::new(),
            // The values the program's own arithmetic produced, one object per partition.
            ends: Ends::Succeeds(vec![
                ("out/partitions/part-00000", "3\n6\n".to_string()),
                ("out/partitions/part-00001", "9\n12\n".to_string()),
                ("out/partitions/_SUCCESS", String::new()),
            ]),
        },
        Row {
            case: "a program that does its work and then raises",
            key: "job/raises_after_work.py",
            token: "settled-raises-after-work",
            source: RAISES_AFTER_ITS_WORK,
            arguments: vec![format!("s3://{BUCKET}/out/raised")],
            absent: Vec::new(),
            // The program's OWN exception, as the engine that ran it reported it, and the
            // objects its work really left behind before it raised.
            ends: Ends::Fails(
                vec!["RuntimeError", "the driver gave up after 10"],
                vec!["out/raised/part-00000"],
            ),
        },
        Row {
            case: "a program that exits with a status of its own",
            key: "job/exits_non_zero.py",
            token: "settled-exits-non-zero",
            source: EXITS_NON_ZERO,
            arguments: Vec::new(),
            // A program that exited non-zero wrote nothing, so nothing is under this prefix.
            absent: vec!["out/exited/part-00000"],
            ends: Ends::Fails(vec!["SystemExit"], Vec::new()),
        },
    ]
}

/// THE RULE: the terminal state a submitted run reports is what RUNNING its work came to, owed
/// to the run from the instant that execution answered — so it is owed before anything has read
/// the run, and the first read taken once the owed span has passed reports the run at rest with
/// the instants and the duration that span really took.
#[tokio::test]
async fn a_submitted_run_comes_to_rest_without_being_polled_for_it() {
    let (router, engine) = emulator();
    given_bucket(&router).await;
    let application_id = started_application(&router, "settled-runs-token").await;

    for row in rows() {
        put_object(&router, row.case, row.key, row.source.as_bytes()).await;
        let submitted = emr_serverless_ok(
            &router,
            row.case,
            "POST",
            &format!("/applications/{application_id}/jobruns"),
            Some(json!({
                "clientToken": row.token,
                "executionRoleArn": EXECUTION_ROLE,
                "jobDriver": {
                    "sparkSubmit": {
                        "entryPoint": format!("s3://{BUCKET}/{}", row.key),
                        "entryPointArguments": row.arguments,
                    }
                },
            })),
        )
        .await;
        let job_run_id = submitted["jobRunId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: StartJobRun answers a jobRunId", row.case))
            .to_string();

        // Nothing reads the run here: the outcome its execution answered is owed to it by the
        // clock, so it is the STORE that shows the terminal state before any call does.
        let owed = terminal_state_owed(&engine, row.case, &application_id, &job_run_id).await;
        make_owed_states_due(&engine, &application_id, &job_run_id);

        // ... and the ONE read this row takes of the run reports it at rest.
        let answered = emr_serverless_ok(
            &router,
            row.case,
            "GET",
            &format!("/applications/{application_id}/jobruns/{job_run_id}"),
            None,
        )
        .await;
        let run = answered["jobRun"].clone();
        let state = run["state"].as_str().unwrap_or_default().to_string();
        assert!(
            TERMINAL_STATES.contains(&state.as_str()),
            "case {:?}: the first read after the run's work answered must report it at rest, \
             got {state:?} with {run}",
            row.case
        );
        assert_eq!(
            state, owed,
            "case {:?}: the state the clock owed the run is the one it reports, expected \
             {owed:?}, got {state:?}",
            row.case
        );

        // "The date and time when the job moved to the RUNNING state" and "when the job was
        // terminated": a run that executed carries both, in that order, and the duration it
        // reports is the span between them.
        let started_at = run["startedAt"].as_f64();
        let ended_at = run["endedAt"].as_f64();
        assert!(
            started_at.is_some() && ended_at.is_some(),
            "case {:?}: a run that executed carries startedAt and endedAt, got {run}",
            row.case
        );
        assert!(
            started_at <= ended_at,
            "case {:?}: expected startedAt {started_at:?} at or before endedAt {ended_at:?}",
            row.case
        );
        let duration = run["totalExecutionDurationSeconds"].as_i64();
        assert!(
            duration.is_some_and(|seconds| seconds >= 1),
            "case {:?}: a run that executed reports the seconds it took, got {duration:?}",
            row.case
        );

        match &row.ends {
            Ends::Succeeds(objects) => {
                assert_eq!(
                    state, "SUCCESS",
                    "case {:?}: a program that exited cleanly succeeds, got {state:?} with \
                     stateDetails {:?}",
                    row.case, run["stateDetails"]
                );
                for (key, content) in objects {
                    let held = object(&router, key).await;
                    assert_eq!(
                        held.as_deref(),
                        Some(content.as_str()),
                        "case {:?}: s3://{BUCKET}/{key} must hold what the program wrote",
                        row.case
                    );
                }
            }
            Ends::Fails(fragments, left_behind) => {
                assert_eq!(
                    state, "FAILED",
                    "case {:?}: a program that failed fails its run, got {state:?}",
                    row.case
                );
                let details = run["stateDetails"].as_str().unwrap_or_default().to_string();
                for fragment in fragments {
                    assert!(
                        details.contains(fragment),
                        "case {:?}: the run's stateDetails must carry the program's own \
                         {fragment:?}, got {details:?}",
                        row.case
                    );
                }
                for key in left_behind {
                    assert!(
                        object(&router, key).await.is_some(),
                        "case {:?}: the work the program did before it failed must leave \
                         s3://{BUCKET}/{key} behind",
                        row.case
                    );
                }
            }
        }

        for key in &row.absent {
            assert!(
                object(&router, key).await.is_none(),
                "case {:?}: a run that wrote nothing leaves no s3://{BUCKET}/{key}",
                row.case
            );
        }
    }
}
