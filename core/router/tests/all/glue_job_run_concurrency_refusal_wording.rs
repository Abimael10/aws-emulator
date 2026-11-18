// SPDX-License-Identifier: CC0-1.0

//! What AWS Glue answers a `StartJobRun` the job's own concurrency ceiling does not admit,
//! across the services the job is stood up through.
//!
//! RULE: a start that would take a job past its effective
//! `ExecutionProperty$MaxConcurrentRuns` — "the maximum number of concurrent runs allowed for
//! the job. The default is 1. An error is returned when this threshold is reached" — counted
//! over that job's runs which have not finished, is refused HTTP 400 with the operation's
//! modelled `ConcurrentRunsExceededException` ("Too many jobs are being run concurrently") and
//! that error class's own FIXED sentence: the wording interpolates nothing from the request or
//! from the stored job — no job name, no count of what is in flight, no ceiling — so a client
//! recognises the refusal by its words whatever it started, and the request and the job's
//! state govern only WHEN it fires, never what it says. A run that has finished holds no slot,
//! so the ceiling bounds what is in flight rather than how many runs were ever started.
//!
//! Only a cross-service suite reaches the recorded shape of this: the job whose ceiling is
//! counted runs a `pythonshell` script Amazon S3 serves, as a role IAM holds and whose trust
//! and grants IAM decides, so the run that holds a slot is a real run of that script and not a
//! record this suite wrote into the store. Each service's own crate can state its half; none
//! of them can state that the refusal a client meets on that job reads the same whatever the
//! job, the ceiling and the runs in flight are.
//!
//! The rows differ along what the rule generalises over: the ceiling the definition carries
//! (left at the documented default, or named), the ceiling an `UpdateJob` moves it to, how many
//! runs are already in flight when the refusal fires, and the shape of the job's own name —
//! including a name that spells the refusal's own words, which a message built from the request
//! would echo.
//!
//! Nothing here waits for a span: the runs holding the slots have the due instants their
//! lifecycle turns on moved into the future THROUGH THE STORE, in a loop that starts another run
//! for a slot the clock freed before the hold reached it, so what a start is measured against is
//! the ceiling's worth of runs in flight however fast the host ran; and the one run this suite
//! lets finish is polled for the CONDITION that it has come to rest, against a generous deadline.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::ComAmazonawsGlueAwsglueState;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is addressed to, and the Region the jobs live in.
const REGION: &str = "us-west-2";

/// The bucket the one script every job here runs lives in.
const BUCKET: &str = "xsvc-glue-concurrency-ceiling";

/// The key of that script, which every job's `ScriptLocation` points at.
const SCRIPT_KEY: &str = "scripts/hold.py";

/// The role every job here runs as: IAM holds it, Glue assumes it, and its one grant is the
/// read of the script above.
const ROLE_NAME: &str = "xsvc-glue-concurrency-role";

/// The modelled error a start past the ceiling is refused with
/// (`smithy_query(query="operation",operation="StartJobRun",model_path="services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json")`:
/// `ConcurrentRunsExceededException`, errorKind client, httpError 400).
const REFUSAL_CODE: &str = "ConcurrentRunsExceededException";

/// The whole sentence that refusal carries, written out here rather than imported from the
/// Glue crate so a wrong constant in the implementation cannot make this suite pass.
const REFUSAL_MESSAGE: &str = "Concurrent runs exceeded.";

/// The HTTP status that refusal is answered with.
const REFUSAL_STATUS: u16 = 400;

/// "The default is 1" (`ExecutionProperty$MaxConcurrentRuns`): the ceiling a job that names no
/// execution property is counted against.
const DEFAULT_CEILING: i64 = 1;

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

/// How long this suite waits for the one run it lets finish, and how often it looks again.
/// Generous: nothing here asserts on how long anything took, and what is waited for is the
/// run's own state rather than a span. Many times over the spans the service owes a run plus the
/// time one script takes, so a host running this beside thousands of other tests delays the wait
/// instead of failing it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
/// See [`SETTLE_DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How far ahead a held run's due instants are moved: far past anything this suite does, so a
/// run that holds a slot goes on holding it for the whole of a row without being waited for.
const HELD_FOR: Duration = Duration::from_secs(600);

/// The script every job here runs: it computes, reads nothing and writes nothing, so a run of
/// it finishes on its own and a row that holds one open holds a real run of a real script.
const SCRIPT: &str = "total = 0\nfor value in range(16):\n    total += value\n";

/// The trust document a role Glue can assume carries.
const GLUE_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"glue.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// Glue, IAM, Amazon S3 and the token service on ONE engine, with the engine the caller keeps:
/// the script a run executes is fetched from Amazon S3 as a session of the job's IAM role, and
/// the store is where a row moves the due instants of the runs it holds open.
fn build_router() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: each call carries a credential scope so the
    // router can name the target service, without a computed signature.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature itself is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDGLUECEILING/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One request against a service's own host, answered as `(status, body)`.
async fn call(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization(signing_name));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// One IAM awsQuery action, which must be served.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, answered) = call(
        router,
        "iam",
        "iam.amazonaws.com",
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        &body,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: IAM {action} must answer 200, got {status} with {answered}"
    );
    answered
}

/// One Amazon S3 REST call, path-style as the emulator serves it.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> (u16, String) {
    call(
        router,
        "s3",
        &format!("s3.{REGION}.amazonaws.com"),
        method,
        path,
        &[("content-type", "application/octet-stream")],
        body,
    )
    .await
}

/// One AWS Glue awsJson1_1 operation, answered as `(status, the parsed document)`.
async fn glue(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let (status, answered) = call(
        router,
        "glue",
        &format!("glue.{REGION}.amazonaws.com"),
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("AWSGlue.{operation}")),
        ],
        &body.to_string(),
    )
    .await;
    (status, serde_json::from_str(&answered).unwrap_or(Value::Null))
}

/// The same, where the call must be served.
async fn glue_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = glue(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Glue {operation} must answer 200, got {status} with {answered}"
    );
    answered
}

/// Create the suite's bucket, stage the one script every job runs in it, and create the role
/// Glue assumes to read it. Answers the role's ARN, as IAM reported it.
async fn given_the_script_and_the_role(router: &MultiServiceRouter) -> String {
    let (status, answered) = s3(
        router,
        "PUT",
        &format!("/{BUCKET}"),
        &format!(
            "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the bucket must be created, got {status} with {answered}"
    );

    let (status, answered) = s3(router, "PUT", &format!("/{BUCKET}/{SCRIPT_KEY}"), SCRIPT).await;
    assert_eq!(
        status, 200,
        "setup: the script must be stored, got {status} with {answered}"
    );

    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", GLUE_TRUST),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    assert!(
        !arn.is_empty(),
        "setup: CreateRole must answer the role's ARN, got {created}"
    );
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", "reads-the-script"),
            ("PolicyDocument", &reads_the_script()),
        ],
    )
    .await;
    arn
}

/// The one grant the role carries: the READ of the script its jobs point at, so a run of one of
/// them is a run whose source Amazon S3 really served it.
fn reads_the_script() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "s3:GetObject",
            "Resource": format!("arn:aws:s3:::{BUCKET}/scripts/*"),
        }],
    })
    .to_string()
}

/// The `JobCommand` every job here carries: the `pythonshell` command of the recorded workflow,
/// pointed at the one staged script.
fn command() -> Value {
    json!({
        "Name": "pythonshell",
        "ScriptLocation": format!("s3://{BUCKET}/{SCRIPT_KEY}"),
        "PythonVersion": "3.9",
    })
}

/// Create `job` as `role`, with the `ExecutionProperty` `ceiling` names — and none at all when
/// it names nothing, which is the case the documented default covers.
async fn create_job(
    router: &MultiServiceRouter,
    case: &str,
    job: &str,
    role: &str,
    ceiling: Option<i64>,
) {
    let mut body = json!({
        "Name": job,
        "Role": role,
        "Command": command(),
        "MaxCapacity": 0.0625,
        "GlueVersion": "3.0",
    });
    if let Some(ceiling) = ceiling {
        body["ExecutionProperty"] = json!({ "MaxConcurrentRuns": ceiling });
    }
    glue_ok(router, case, "CreateJob", &body).await;
}

/// Move `job`'s ceiling to `ceiling` through `UpdateJob`, which replaces the definition's
/// members with the ones the update carries.
async fn move_the_ceiling(
    router: &MultiServiceRouter,
    case: &str,
    job: &str,
    role: &str,
    ceiling: i64,
) {
    glue_ok(
        router,
        case,
        "UpdateJob",
        &json!({
            "JobName": job,
            "JobUpdate": {
                "Role": role,
                "Command": command(),
                "MaxCapacity": 0.0625,
                "GlueVersion": "3.0",
                "ExecutionProperty": { "MaxConcurrentRuns": ceiling },
            },
        }),
    )
    .await;
}

/// The ceiling `GetJob` reports for `job`: the figure a start is measured against.
async fn reported_ceiling(router: &MultiServiceRouter, case: &str, job: &str) -> i64 {
    let body = glue_ok(router, case, "GetJob", &json!({ "JobName": job })).await;
    body.get("Job")
        .and_then(|job| job.get("ExecutionProperty"))
        .and_then(|property| property.get("MaxConcurrentRuns"))
        .and_then(Value::as_i64)
        .unwrap_or_else(|| {
            panic!("case {case:?}: GetJob must report the job's MaxConcurrentRuns, got {body}")
        })
}

/// The ids `GetJobRuns` reports for `job` — where a run a refused start had minted would show
/// up.
async fn listed_run_ids(router: &MultiServiceRouter, case: &str, job: &str) -> BTreeSet<String> {
    let page = glue_ok(router, case, "GetJobRuns", &json!({ "JobName": job })).await;
    page.get("JobRuns")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: GetJobRuns must report JobRuns, got {page}"))
        .iter()
        .filter_map(|run| run.get("Id").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// Start one run of `job`, which must be admitted, and answer the id it minted.
async fn start_run(router: &MultiServiceRouter, case: &str, job: &str) -> String {
    let started = glue_ok(router, case, "StartJobRun", &json!({ "JobName": job })).await;
    started
        .get("JobRunId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: StartJobRun must answer a JobRunId, got {started}")
        })
        .to_string()
}

/// Hold every unfinished run of `job` where it is, by moving the due instants its lifecycle
/// turns on past anything this suite does.
///
/// A run owes up to TWO states at once — the state it is in while its script runs, and the
/// state its own execution comes to rest in — so both instants move, or a run would come to
/// rest half way and free the slot it holds. A finished run owes neither and is left alone.
fn hold_the_runs_of(engine: &Arc<engine::InMemoryEngine>, case: &str, job: &str) {
    let held = SystemTime::now() + HELD_FOR;
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    let entry = store
        .job_mut(REGION, job)
        .unwrap_or_else(|| panic!("case {case:?}: the store must hold the job {job:?}"));
    for run in &mut entry.runs {
        if run.settles_at.is_some() {
            run.settles_at = Some(held);
        }
        if run.rests_at.is_some() {
            run.rests_at = Some(held);
        }
    }
}

/// How many runs of `job` the store counts as being in flight: the runs of it that have not
/// finished, which is the figure a start is measured against.
fn runs_in_flight(engine: &Arc<engine::InMemoryEngine>, case: &str, job: &str) -> usize {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let store = state.lock();
    store
        .job(REGION, job)
        .unwrap_or_else(|| panic!("case {case:?}: the store must hold the job {job:?}"))
        .runs_in_flight()
}

/// Bring `job` to exactly `ceiling` runs in flight and hold them there, adding every run it
/// starts to `held`.
///
/// A run's lifecycle is driven by the CLOCK, so a run started here may come to rest before the
/// hold reaches it — freeing the slot it held — and how likely that is depends on how fast the
/// host runs. The loop takes that out of the outcome: it holds what the job still owes, counts
/// what is in flight, and starts another run for a slot that fell free, so what the next start
/// is measured against is `ceiling` runs in flight whatever the host did. The hold moves only
/// the instants a run still owes, so a run that has finished stays finished and is not counted.
async fn hold_at_the_ceiling(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    job: &str,
    ceiling: usize,
    held: &mut BTreeSet<String>,
) {
    // One iteration per slot to fill, plus room for the slots a host slow enough to settle a
    // run mid-row frees again. A bound, not a deadline: it counts starts, not time.
    let mut remaining = ceiling + 32;
    loop {
        hold_the_runs_of(engine, case, job);
        let in_flight = runs_in_flight(engine, case, job);
        assert!(
            in_flight <= ceiling,
            "case {case:?}: the job must never hold more than {ceiling} runs in flight, got \
             {in_flight}"
        );
        if in_flight == ceiling {
            return;
        }
        assert!(
            remaining > 0,
            "case {case:?}: the job could not be brought to {ceiling} runs in flight; it holds \
             {in_flight}"
        );
        remaining -= 1;
        held.insert(start_run(router, case, job).await);
    }
}

/// Assert that one more start of `job` is refused for concurrency, that the refusal says it in
/// the service's own fixed words and that it left the job holding exactly `admitted`. Answers
/// the sentence it was refused with, so the caller can compare the sentences of every row.
async fn refused_start_message(
    router: &MultiServiceRouter,
    case: &str,
    job: &str,
    admitted: &BTreeSet<String>,
) -> String {
    let (status, answer) = glue(router, "StartJobRun", &json!({ "JobName": job })).await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case {case:?}: the start past the job's ceiling must be refused {REFUSAL_STATUS}, got \
         {status} with {answer}"
    );
    let code = answer
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        code.contains(REFUSAL_CODE),
        "case {case:?}: expected {REFUSAL_CODE}, got {answer}"
    );
    let message = answer
        .get("Message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // What the sentence must NOT carry comes first, so a message built from the request or from
    // the stored job is reported as the interpolation it is rather than as a text mismatch: such
    // a message carries the job's name, and one built from the figures carries a digit.
    assert!(
        !message.contains(job),
        "case {case:?}: the refusal must not name the job {job:?}, got {message:?}"
    );
    assert!(
        !message.chars().any(|character| character.is_ascii_digit()),
        "case {case:?}: the refusal must carry no count and no ceiling, got {message:?}"
    );
    assert_eq!(
        message, REFUSAL_MESSAGE,
        "case {case:?}: expected the fixed concurrency refusal {REFUSAL_MESSAGE:?}, got \
         {message:?}"
    );
    let listed = listed_run_ids(router, case, job).await;
    assert_eq!(
        &listed, admitted,
        "case {case:?}: a refused start must mint no run, and the job's runs are {listed:?}"
    );
    message
}

/// One row: the job, the ceiling it is created with and the ceiling an update moves it to.
struct Row {
    /// What every assertion of the row reports.
    case: &'static str,
    /// The job's own name — one of them spelling the refusal's own words, so a sentence built
    /// from the request would be told apart here.
    job: &'static str,
    /// The `ExecutionProperty$MaxConcurrentRuns` the create names, or `None` for a create that
    /// names no execution property at all.
    created: Option<i64>,
    /// The ceiling an `UpdateJob` then moves it to, or `None` for a row that moves nothing.
    raised_to: Option<i64>,
}

/// The rows: the documented default raised to two (the recorded shape), three runs raised to
/// five, and a job whose own name is the refusal's words. The ceilings a definition is simply
/// CREATED with are tabled by the Glue crate's own suite
/// (`services/service-com-amazonaws-glue-awsglue/tests/all/job_runs.rs`); what these rows add is
/// the ceiling an update moves, the counts a figure-bearing sentence would interpolate, and a
/// name the refusal would echo.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the documented default, raised to two",
            job: "xsvc_concurrency_default",
            created: None,
            raised_to: Some(2),
        },
        Row {
            case: "three concurrent runs, raised to five",
            job: "xsvc_concurrency_three",
            created: Some(3),
            raised_to: Some(5),
        },
        Row {
            case: "a job whose name is the refusal's own words",
            job: "Concurrent runs exceeded 7",
            created: Some(1),
            raised_to: None,
        },
    ]
}

/// A start the job's current ceiling does not admit is refused with the modelled concurrency
/// error and ONE sentence: the same text for every job, every ceiling and every count of runs
/// in flight, with nothing of the request or of the stored job in it.
#[tokio::test]
async fn a_start_past_the_jobs_ceiling_is_refused_in_the_services_own_fixed_words() {
    let (router, engine) = build_router();
    let role = given_the_script_and_the_role(&router).await;

    let mut sentences: BTreeSet<String> = BTreeSet::new();
    for row in rows() {
        let case = row.case;
        create_job(&router, case, row.job, &role, row.created).await;
        let ceiling = row.created.unwrap_or(DEFAULT_CEILING);
        assert_eq!(
            reported_ceiling(&router, case, row.job).await,
            ceiling,
            "case {case:?}: the created definition must report the ceiling this row counts \
             against"
        );

        // The runs are held in flight, so all of them occupy a slot at once: the ceiling bounds
        // what is IN FLIGHT, which is what the next start is measured against.
        let ceiling_held = usize::try_from(ceiling).expect("a row's ceiling is a count");
        let mut admitted: BTreeSet<String> = BTreeSet::new();
        hold_at_the_ceiling(
            &router,
            &engine,
            case,
            row.job,
            ceiling_held,
            &mut admitted,
        )
        .await;
        assert_eq!(
            listed_run_ids(&router, case, row.job).await,
            admitted,
            "case {case:?}: the job must hold exactly the runs its admitted starts minted"
        );
        sentences.insert(refused_start_message(&router, case, row.job, &admitted).await);

        // An update that moves the ceiling moves WHEN the refusal fires, and not a word of
        // what it says: the starts the raised ceiling admits are served, and the one past it
        // is refused with the same sentence.
        if let Some(raised_to) = row.raised_to {
            move_the_ceiling(&router, case, row.job, &role, raised_to).await;
            assert_eq!(
                reported_ceiling(&router, case, row.job).await,
                raised_to,
                "case {case:?}: the updated definition must report the ceiling the rest of this \
                 row counts against"
            );
            let raised_held = usize::try_from(raised_to).expect("a row's ceiling is a count");
            let before = admitted.len();
            hold_at_the_ceiling(
                &router,
                &engine,
                case,
                row.job,
                raised_held,
                &mut admitted,
            )
            .await;
            assert!(
                admitted.len() >= raised_held && admitted.len() > before,
                "case {case:?}: the raised ceiling must admit the starts the old one refused, \
                 and the job holds {} run(s)",
                admitted.len()
            );
            assert_eq!(
                listed_run_ids(&router, case, row.job).await,
                admitted,
                "case {case:?}: the starts the raised ceiling admits must be recorded"
            );
            sentences.insert(refused_start_message(&router, case, row.job, &admitted).await);
        }
    }

    assert_eq!(
        sentences,
        BTreeSet::from([REFUSAL_MESSAGE.to_string()]),
        "every row must be refused with the one sentence of this error class, got {sentences:?}"
    );
}

/// The ceiling bounds the runs IN FLIGHT and not the runs ever started: a run that has come to
/// rest frees its slot, so the next start of a single-run job is served — and once that start's
/// run is held in flight, the one after it is refused with the same fixed sentence.
///
/// The run this lets finish is the only REAL one in the suite, and the reason the suite stands
/// the job up through the other services at all: its script is the object Amazon S3 serves, read
/// as a session of the role IAM holds, so the slot that frees is freed by a script this emulator
/// really ran rather than by a record written into the store. It is polled for the condition that
/// the run has come to rest.
#[tokio::test]
async fn a_finished_run_frees_the_slot_it_held() {
    let (router, engine) = build_router();
    let role = given_the_script_and_the_role(&router).await;
    let case = "a single-run job whose run finishes";
    let job = "xsvc_concurrency_reuse";
    create_job(&router, case, job, &role, Some(1)).await;

    let first = start_run(&router, case, job).await;
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let at_rest = loop {
        let reported = glue_ok(
            &router,
            case,
            "GetJobRun",
            &json!({ "JobName": job, "RunId": first }),
        )
        .await;
        let run = reported["JobRun"].clone();
        let state = run["JobRunState"].as_str().unwrap_or_default().to_string();
        if TERMINAL_RUN_STATES.contains(&state.as_str()) {
            break run;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the run never came to rest; it reports {run}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        at_rest["JobRunState"].as_str(),
        Some("SUCCEEDED"),
        "case {case:?}: the run of a script that computes and reads nothing must succeed, got \
         {at_rest}"
    );

    // The slot the finished run held is free, so the next start is SERVED even though the job
    // admits one run at a time and two runs then exist.
    let mut admitted: BTreeSet<String> = BTreeSet::from([first]);
    hold_at_the_ceiling(&router, &engine, case, job, 1, &mut admitted).await;
    assert!(
        admitted.len() >= 2,
        "case {case:?}: the finished run's slot must have been free for another start, and the \
         job holds {} run(s)",
        admitted.len()
    );
    assert_eq!(
        listed_run_ids(&router, case, job).await,
        admitted,
        "case {case:?}: the job must hold the finished run and the held one"
    );
    refused_start_message(&router, case, job, &admitted).await;
}
