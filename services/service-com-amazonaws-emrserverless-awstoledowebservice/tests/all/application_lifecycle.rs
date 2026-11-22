// SPDX-License-Identifier: CC0-1.0

//! The EMR Serverless application and job run lifecycle, end to end over HTTP.
//!
//! Every assertion goes through the real router (restJson1, SigV4 signing name
//! `emr-serverless`), so each case exercises routing, deserialization, the handler, the
//! persisted record and serialization — the same path an SDK client takes. The rule
//! under test is the one the control plane states for its resources: an application and
//! a job run are whatever the create or submit stored, and every reporting operation
//! computes its answer from that record rather than from the request that asked.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_emrserverless_awstoledowebservice::state::spark_job_run::JOB_RUN_UNREADABLE_LEAD;
use service_com_amazonaws_emrserverless_awstoledowebservice::state::{
    non_terminal_job_run_window, DEFAULT_EXECUTION_TIMEOUT_MINUTES, JOB_RUN_STAGES,
    JOB_RUN_STAGE_TRANSITION, LIFECYCLE_TRANSITION, PAGE_LIMIT, TERMINAL_JOB_RUN_STATES,
};
use service_com_amazonaws_emrserverless_awstoledowebservice::EmulatedService;

/// The region the test requests are signed for; it is the region every minted ARN
/// names.
const REGION: &str = "eu-west-1";

/// A SigV4 credential scope naming this service's signing name (`emr-serverless`,
/// from `smithy_query(query="service_info")`). It is what routes a request here;
/// request authentication is off, so the signature itself is never checked.
const SIGNED_FOR_EU_WEST_1: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/emr-serverless/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The account id every emulator ARN carries (`engine::emulator_account_id`).
const ACCOUNT_ID: &str = "000000000000";

/// Long enough for a clock-owed transition to have fallen due.
pub(crate) fn wait_for_transition() {
    std::thread::sleep(LIFECYCLE_TRANSITION + Duration::from_millis(250));
}

/// Long enough for EVERY span the clock owes a submitted job run to have fallen due: one
/// `JOB_RUN_STAGE_TRANSITION` per stage of `JOB_RUN_STAGES`, and one more for the state the
/// run's own execution answered, which falls due a span after the last of them.
pub(crate) fn wait_for_job_run_to_settle() {
    let spans = u32::try_from(JOB_RUN_STAGES.len()).unwrap_or(1) + 1;
    std::thread::sleep(JOB_RUN_STAGE_TRANSITION * spans + Duration::from_millis(250));
}

/// How long a test looks for a job run to come to REST before giving up.
///
/// Generous: the run rests on what RUNNING its submitted application came to, which is work
/// of this emulator's own and not a span anybody waits out, so the deadline is far above what
/// any of it takes on an idle host or a loaded one.
pub(crate) const JOB_RUN_REST_DEADLINE: Duration = Duration::from_secs(120);
/// How long a poll waits before looking again.
const JOB_RUN_REST_POLL: Duration = Duration::from_millis(100);

/// The job run `job_run_id`, read once it has come to REST — polled for the CONDITION rather
/// than waited out, and panicking only past `JOB_RUN_REST_DEADLINE`.
pub(crate) async fn settled_job_run(
    router: &MultiServiceRouter,
    application_id: &str,
    job_run_id: &str,
) -> Answer {
    let deadline = Instant::now() + JOB_RUN_REST_DEADLINE;
    loop {
        let answer = get_job_run(router, application_id, job_run_id).await;
        let last = answer
            .at("/jobRun/state")
            .as_str()
            .unwrap_or_default()
            .to_string();
        if TERMINAL_JOB_RUN_STATES.contains(&last.as_str()) {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "job run {job_run_id} never came to rest, last saw {last:?}"
        );
        tokio::time::sleep(JOB_RUN_REST_POLL).await;
    }
}

/// One emulator: this service beside the object store, over one shared engine, with the
/// engine handed back so a test can read or rewind the state the handlers share.
///
/// Amazon S3 is mounted beside it because a job run's submitted application is READ from the
/// object store before anything runs it — what the assembled emulator does — so a run of a
/// submit whose entry point the store does not hold is answered by the store itself rather
/// than by a call nothing served.
pub(crate) fn emulator_with_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// One emulator: this service beside the object store, over one shared engine.
pub(crate) fn emulator() -> MultiServiceRouter {
    emulator_with_engine().0
}

/// What the emulator answered: the HTTP status, the `x-amzn-errortype` identity and
/// the JSON body.
pub(crate) struct Answer {
    pub(crate) status: u16,
    /// The `x-amzn-errortype` header, which is how restJson1 names the error shape a
    /// client branches on. `None` on a success, which carries no such header.
    pub(crate) error_type: Option<String>,
    pub(crate) body: Value,
}

impl Answer {
    /// The wire error code (`__type`), or the empty string on a success.
    pub(crate) fn error_code(&self) -> &str {
        // restJson1 renders the error shape id in `__type`, which may be namespaced.
        let raw = self.body["__type"].as_str().unwrap_or_default();
        raw.rsplit('#').next().unwrap_or_default()
    }

    /// The error SHAPE a restJson1 client resolves from the `x-amzn-errortype`
    /// identity, or `None` when the answer carries no identity.
    ///
    /// The protocol admits three forms — `Shape`, `namespace#Shape` and
    /// `Shape:<namespace URI>` — and a client takes the name after the last `#` and
    /// before the first `:`, so this is exactly what a client's `catch` keys off.
    pub(crate) fn resolved_error_shape(&self) -> Option<&str> {
        self.error_type.as_deref().map(|identity| {
            let after_namespace = identity.rsplit('#').next().unwrap_or(identity);
            after_namespace.split(':').next().unwrap_or(after_namespace)
        })
    }

    /// The value at a JSON pointer, or `Value::Null`.
    pub(crate) fn at(&self, pointer: &str) -> Value {
        self.body.pointer(pointer).cloned().unwrap_or(Value::Null)
    }
}

/// One signed request, answered as the WHOLE HTTP response, so a suite can assert on the
/// status, the body BYTES and the response headers — the framing a parsed [`Answer`]
/// cannot show. [`call`] is this with the body parsed as JSON.
pub(crate) async fn call_http(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> ::http::Response<Bytes> {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "emr-serverless.eu-west-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_EU_WEST_1);
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    router.dispatch(request).await
}

pub(crate) async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> Answer {
    let response = call_http(router, method, uri, body).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer {
        status,
        error_type,
        body,
    }
}

pub(crate) async fn create_application(router: &MultiServiceRouter, request: Value) -> Answer {
    call(router, "POST", "/applications", Some(request)).await
}

pub(crate) async fn get_application(router: &MultiServiceRouter, id: &str) -> Answer {
    call(router, "GET", &format!("/applications/{id}"), None).await
}

pub(crate) async fn list_applications(router: &MultiServiceRouter, query: &str) -> Answer {
    call(router, "GET", &format!("/applications{query}"), None).await
}

pub(crate) async fn start_application(router: &MultiServiceRouter, id: &str) -> Answer {
    call(router, "POST", &format!("/applications/{id}/start"), None).await
}

pub(crate) async fn stop_application(router: &MultiServiceRouter, id: &str) -> Answer {
    call(router, "POST", &format!("/applications/{id}/stop"), None).await
}

pub(crate) async fn delete_application(router: &MultiServiceRouter, id: &str) -> Answer {
    call(router, "DELETE", &format!("/applications/{id}"), None).await
}

pub(crate) async fn start_job_run(
    router: &MultiServiceRouter,
    id: &str,
    request: Value,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/applications/{id}/jobruns"),
        Some(request),
    )
    .await
}

pub(crate) async fn get_job_run(
    router: &MultiServiceRouter,
    id: &str,
    job_run_id: &str,
) -> Answer {
    call(
        router,
        "GET",
        &format!("/applications/{id}/jobruns/{job_run_id}"),
        None,
    )
    .await
}

pub(crate) async fn list_job_runs(
    router: &MultiServiceRouter,
    id: &str,
    query: &str,
) -> Answer {
    call(
        router,
        "GET",
        &format!("/applications/{id}/jobruns{query}"),
        None,
    )
    .await
}

pub(crate) async fn cancel_job_run(
    router: &MultiServiceRouter,
    id: &str,
    job_run_id: &str,
    query: &str,
) -> Answer {
    call(
        router,
        "DELETE",
        &format!("/applications/{id}/jobruns/{job_run_id}{query}"),
        None,
    )
    .await
}

/// `ApplicationId` / `JobRunId`'s modelled `@pattern` `^[0-9a-z]+$` with length 1..64,
/// checked by hand (this crate has no regex dependency) rather than through the crate's
/// own predicate, so a wrong predicate in `src/` cannot make this test pass. Source:
/// `smithy_query(query="shape",shape="Application")`.
pub(crate) fn matches_resource_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
}

/// `ApplicationArn`'s modelled `@pattern`
/// `^arn:(aws[a-zA-Z0-9-]*):emr-serverless:.+:(\d{12}):\/applications\/[0-9a-zA-Z]+$`
/// with length 60..1024.
fn matches_application_arn(arn: &str, region: &str) -> bool {
    let expected_prefix = format!("arn:aws:emr-serverless:{region}:{ACCOUNT_ID}:/applications/");
    let Some(id) = arn.strip_prefix(&expected_prefix) else {
        return false;
    };
    !id.is_empty()
        && id.chars().all(|c| c.is_ascii_alphanumeric())
        && (60..=1024).contains(&arn.len())
}

/// `JobArn`'s modelled `@pattern`: an application ARN followed by
/// `/jobruns/<jobRunId>`.
fn matches_job_run_arn(arn: &str, region: &str) -> bool {
    let Some((application_part, job_run_id)) = arn.rsplit_once("/jobruns/") else {
        return false;
    };
    matches_application_arn(application_part, region)
        && !job_run_id.is_empty()
        && job_run_id.chars().all(|c| c.is_ascii_alphanumeric())
        && (60..=1024).contains(&arn.len())
}

/// `RequestIdentityUserArn`'s modelled `@pattern`
/// `^arn:(aws[a-zA-Z0-9-]*):(iam|sts)::(\d{12})?:[\w/+=,.@-]+$` with length 20..2048.
pub(crate) fn matches_request_identity_arn(arn: &str) -> bool {
    let Some(rest) = arn
        .strip_prefix("arn:aws:iam::")
        .or_else(|| arn.strip_prefix("arn:aws:sts::"))
    else {
        return false;
    };
    let Some((account, resource)) = rest.split_once(':') else {
        return false;
    };
    let account_ok = account.is_empty()
        || (account.len() == 12 && account.chars().all(|c| c.is_ascii_digit()));
    account_ok
        && !resource.is_empty()
        && resource.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '+' | '=' | ',' | '.' | '@' | '-')
        })
        && (20..=2048).contains(&arn.len())
}

/// `NextToken`'s modelled `@pattern` `^[A-Za-z0-9_=-]+$` with length 1..1024.
fn matches_next_token(token: &str) -> bool {
    (1..=1024).contains(&token.len())
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '=' | '-'))
}

/// A minimal `CreateApplication` body: only the three required members, with the
/// client token the caller names.
pub(crate) fn minimal_create(token: &str) -> Value {
    json!({
        "clientToken": token,
        "releaseLabel": "emr-7.1.0",
        "type": "SPARK",
    })
}

/// A `sparkSubmit` job driver (the `JobDriver` union's `sparkSubmit` member).
fn spark_submit_driver(entry_point: &str) -> Value {
    json!({ "sparkSubmit": { "entryPoint": entry_point } })
}

/// An execution role ARN satisfying `IAMRoleArn`'s pattern.
pub(crate) const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/EMRServerlessRole";

/// A minimal `StartJobRun` body.
pub(crate) fn minimal_job_run(token: &str) -> Value {
    json!({
        "clientToken": token,
        "executionRoleArn": EXECUTION_ROLE,
        "jobDriver": spark_submit_driver("s3://bucket/app.py"),
    })
}

/// Create an application and return its id, waiting out the create transition so the
/// application is `CREATED`.
pub(crate) async fn created_application(router: &MultiServiceRouter, token: &str) -> String {
    let answer = create_application(router, minimal_create(token)).await;
    assert_eq!(
        answer.status, 200,
        "case {token:?}: CreateApplication expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    let id = answer.at("/applicationId").as_str().unwrap_or("").to_string();
    wait_for_transition();
    id
}

/// Create an application and bring it to `STARTED`.
pub(crate) async fn started_application(router: &MultiServiceRouter, token: &str) -> String {
    let id = created_application(router, token).await;
    let started = start_application(router, &id).await;
    assert_eq!(
        started.status, 200,
        "case {token:?}: StartApplication expected 200, got {} with {:?}",
        started.status, started.body
    );
    wait_for_transition();
    id
}

/// The whole lifecycle the issue asks for, in order, with every answer computed from
/// the persisted record.
#[tokio::test]
async fn the_application_and_job_run_lifecycle_round_trips_through_state() {
    let router = emulator();

    let created = create_application(
        &router,
        json!({
            "clientToken": "tok-lifecycle",
            "releaseLabel": "emr-7.1.0",
            "type": "SPARK",
            "name": "demo",
            "tags": { "team": "data" },
        }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "CreateApplication expected 200, got {} with {:?}",
        created.status, created.body
    );
    let application_id = created
        .at("/applicationId")
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        matches_resource_id(&application_id),
        "case lifecycle: applicationId must match ^[0-9a-z]+$, got {application_id:?}"
    );
    let application_arn = created.at("/arn").as_str().unwrap_or_default().to_string();
    assert!(
        matches_application_arn(&application_arn, REGION),
        "case lifecycle: arn must match ApplicationArn's pattern, got {application_arn:?}"
    );
    assert_eq!(
        created.at("/name"),
        json!("demo"),
        "case lifecycle: CreateApplication returns the stored name, got {:?}",
        created.at("/name")
    );

    wait_for_transition();

    let fetched = get_application(&router, &application_id).await;
    assert_eq!(
        fetched.status, 200,
        "GetApplication expected 200, got {} with {:?}",
        fetched.status, fetched.body
    );
    for (member, expected) in [
        ("/application/applicationId", json!(application_id)),
        ("/application/arn", json!(application_arn)),
        ("/application/name", json!("demo")),
        ("/application/releaseLabel", json!("emr-7.1.0")),
        // The engine type is reported in the service's canonical casing, not the
        // casing the create asked in.
        ("/application/type", json!("Spark")),
        ("/application/state", json!("CREATED")),
        ("/application/tags/team", json!("data")),
    ] {
        assert_eq!(
            fetched.at(member),
            expected,
            "case lifecycle: GetApplication{member} expected {expected:?}, got {:?}",
            fetched.at(member)
        );
    }

    let started = start_application(&router, &application_id).await;
    assert_eq!(
        started.status, 200,
        "StartApplication expected 200, got {} with {:?}",
        started.status, started.body
    );
    wait_for_transition();
    let after_start = get_application(&router, &application_id).await;
    assert_eq!(
        after_start.at("/application/state"),
        json!("STARTED"),
        "case lifecycle: the started application reports STARTED, got {:?}",
        after_start.at("/application/state")
    );
    assert!(
        after_start.at("/application/updatedAt").as_f64()
            >= fetched.at("/application/updatedAt").as_f64(),
        "case lifecycle: updatedAt must not go backwards, got {:?} after {:?}",
        after_start.at("/application/updatedAt"),
        fetched.at("/application/updatedAt")
    );

    let submitted = start_job_run(
        &router,
        &application_id,
        json!({
            "clientToken": "job-1",
            "executionRoleArn": EXECUTION_ROLE,
            "jobDriver": spark_submit_driver("s3://bucket/app.py"),
            "name": "nightly",
            "mode": "BATCH",
        }),
    )
    .await;
    assert_eq!(
        submitted.status, 200,
        "StartJobRun expected 200, got {} with {:?}",
        submitted.status, submitted.body
    );
    let job_run_id = submitted
        .at("/jobRunId")
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        matches_resource_id(&job_run_id),
        "case lifecycle: jobRunId must match ^[0-9a-z]+$, got {job_run_id:?}"
    );
    let job_run_arn = submitted.at("/arn").as_str().unwrap_or_default().to_string();
    assert!(
        matches_job_run_arn(&job_run_arn, REGION),
        "case lifecycle: arn must match JobArn's pattern, got {job_run_arn:?}"
    );
    assert_eq!(
        submitted.at("/applicationId"),
        json!(application_id),
        "case lifecycle: StartJobRun reports its own application, got {:?}",
        submitted.at("/applicationId")
    );

    let run = get_job_run(&router, &application_id, &job_run_id).await;
    assert_eq!(
        run.status, 200,
        "GetJobRun expected 200, got {} with {:?}",
        run.status, run.body
    );
    for (member, expected) in [
        ("/jobRun/applicationId", json!(application_id)),
        ("/jobRun/jobRunId", json!(job_run_id)),
        ("/jobRun/arn", json!(job_run_arn)),
        ("/jobRun/name", json!("nightly")),
        ("/jobRun/mode", json!("BATCH")),
        ("/jobRun/executionRole", json!(EXECUTION_ROLE)),
        // The release label is the APPLICATION's, never the request's.
        ("/jobRun/releaseLabel", json!("emr-7.1.0")),
        ("/jobRun/attempt", json!(1)),
        (
            "/jobRun/jobDriver/sparkSubmit/entryPoint",
            json!("s3://bucket/app.py"),
        ),
    ] {
        assert_eq!(
            run.at(member),
            expected,
            "case lifecycle: GetJobRun{member} expected {expected:?}, got {:?}",
            run.at(member)
        );
    }
    let created_by = run.at("/jobRun/createdBy");
    assert!(
        matches_request_identity_arn(created_by.as_str().unwrap_or_default()),
        "case lifecycle: createdBy must match RequestIdentityUserArn's pattern, got {created_by:?}"
    );

    let listed_runs = list_job_runs(&router, &application_id, "").await;
    assert_eq!(
        listed_runs.at("/jobRuns/0/id"),
        json!(job_run_id),
        "case lifecycle: ListJobRuns reports the submitted run, got {:?}",
        listed_runs.body
    );
    assert_eq!(
        listed_runs.at("/jobRuns/0/releaseLabel"),
        json!("emr-7.1.0"),
        "case lifecycle: the summary carries the stored release label, got {:?}",
        listed_runs.at("/jobRuns/0/releaseLabel")
    );

    let cancelled = cancel_job_run(&router, &application_id, &job_run_id, "").await;
    assert_eq!(
        cancelled.status, 200,
        "CancelJobRun expected 200, got {} with {:?}",
        cancelled.status, cancelled.body
    );
    assert_eq!(
        cancelled.at("/jobRunId"),
        json!(job_run_id),
        "case lifecycle: CancelJobRun names the run it cancelled, got {:?}",
        cancelled.body
    );
    wait_for_transition();
    let after_cancel = get_job_run(&router, &application_id, &job_run_id).await;
    assert_eq!(
        after_cancel.at("/jobRun/state"),
        json!("CANCELLED"),
        "case lifecycle: the cancelled run settles to CANCELLED, got {:?}",
        after_cancel.at("/jobRun/state")
    );

    let stopped = stop_application(&router, &application_id).await;
    assert_eq!(
        stopped.status, 200,
        "StopApplication expected 200, got {} with {:?}",
        stopped.status, stopped.body
    );
    wait_for_transition();
    let after_stop = get_application(&router, &application_id).await;
    assert_eq!(
        after_stop.at("/application/state"),
        json!("STOPPED"),
        "case lifecycle: the stopped application settles to STOPPED, got {:?}",
        after_stop.at("/application/state")
    );

    let deleted = delete_application(&router, &application_id).await;
    assert_eq!(
        deleted.status, 200,
        "DeleteApplication expected 200, got {} with {:?}",
        deleted.status, deleted.body
    );
    // The deleted application is RETAINED in the terminal state of the
    // `ApplicationState` enum, so a client polling it observes `TERMINATED` instead of
    // the state it last had.
    let gone = get_application(&router, &application_id).await;
    assert_eq!(
        gone.status, 200,
        "case lifecycle: GetApplication after the delete expected 200, got {} with {:?}",
        gone.status, gone.body
    );
    assert_eq!(
        gone.at("/application/state"),
        json!("TERMINATED"),
        "case lifecycle: the deleted application reports TERMINATED, got {:?}",
        gone.at("/application/state")
    );
    let remaining = list_applications(&router, "?states=TERMINATED").await;
    assert_eq!(
        remaining.at("/applications/0/id"),
        json!(application_id),
        "case lifecycle: the terminated application is listed under its state, got {:?}",
        remaining.body
    );
}

/// Whatever a create stores, a later read reports unchanged — across values, sizes,
/// types, counts and forms of the optional members.
#[tokio::test]
async fn get_application_reports_exactly_what_the_create_stored() {
    // The `type` values are `EngineType` (a free string, len 1..64) and the two
    // engines EMR Serverless documents; the `architecture` values are the whole
    // `Architecture` enum (`ARCHITECTURE_VALUES`); the capacity strings satisfy
    // `CpuSize` / `MemorySize` / `DiskSize`'s patterns.
    let cases: Vec<(&str, Value)> = vec![
        ("only the required members", minimal_create("tok-minimal")),
        (
            "a name and one tag",
            json!({
                "clientToken": "tok-named",
                "releaseLabel": "emr-6.15.0",
                "type": "HIVE",
                "name": "etl.nightly",
                "tags": { "team": "data" },
            }),
        ),
        (
            "three tags and the ARM64 architecture",
            json!({
                "clientToken": "tok-arm",
                "releaseLabel": "emr-7.2.0",
                "type": "SPARK",
                "architecture": "ARM64",
                "tags": { "a": "1", "b": "2", "c": "3" },
            }),
        ),
        (
            "the X86_64 architecture and a maximum capacity",
            json!({
                "clientToken": "tok-x86",
                "releaseLabel": "emr-7.0.0",
                "type": "SPARK",
                "architecture": "X86_64",
                "maximumCapacity": { "cpu": "400vCPU", "memory": "3000GB", "disk": "20000GB" },
            }),
        ),
        (
            "two initial capacity entries with worker configurations",
            json!({
                "clientToken": "tok-capacity",
                "releaseLabel": "emr-7.1.0",
                "type": "SPARK",
                "initialCapacity": {
                    "Driver": {
                        "workerCount": 1,
                        "workerConfiguration": { "cpu": "2vCPU", "memory": "4GB" },
                    },
                    "Executor": {
                        "workerCount": 4,
                        "workerConfiguration": {
                            "cpu": "4vCPU",
                            "memory": "8GB",
                            "disk": "20GB",
                            "diskType": "SHUFFLE_OPTIMIZED",
                        },
                    },
                },
            }),
        ),
        (
            "auto start and auto stop given explicitly",
            json!({
                "clientToken": "tok-auto",
                "releaseLabel": "emr-7.1.0",
                "type": "SPARK",
                "autoStartConfiguration": { "enabled": false },
                "autoStopConfiguration": { "enabled": false, "idleTimeoutMinutes": 120 },
            }),
        ),
        (
            "a runtime configuration list and a network configuration",
            json!({
                "clientToken": "tok-runtime",
                "releaseLabel": "emr-7.1.0",
                "type": "SPARK",
                "runtimeConfiguration": [
                    { "classification": "spark-defaults", "properties": { "spark.executor.cores": "2" } },
                ],
                "networkConfiguration": {
                    "subnetIds": ["subnet-0a1b2c3d", "subnet-4e5f6a7b"],
                    "securityGroupIds": ["sg-0a1b2c3d"],
                },
            }),
        ),
    ];

    let router = emulator();
    for (case, request) in cases {
        let created = create_application(&router, request.clone()).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateApplication expected 200, got {} with {:?}",
            created.status, created.body
        );
        let id = created
            .at("/applicationId")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            matches_resource_id(&id),
            "case {case:?}: applicationId must match ^[0-9a-z]+$, got {id:?}"
        );

        let fetched = get_application(&router, &id).await;
        assert_eq!(
            fetched.status, 200,
            "case {case:?}: GetApplication expected 200, got {} with {:?}",
            fetched.status, fetched.body
        );

        // Every member the request named reads back identical.
        let members = request.as_object().expect("the request is an object");
        for (member, expected) in members {
            if member == "clientToken" {
                // `clientToken` is an idempotency key, not a member of `Application`.
                continue;
            }
            let pointer = format!("/application/{member}");
            if member == "autoStartConfiguration" || member == "autoStopConfiguration" {
                // Checked below together with their documented defaults.
                continue;
            }
            if member == "type" {
                // The engine type reads back in the service's canonical casing, which
                // `an_engine_type_reads_back_in_the_services_own_casing` pins; here it
                // only has to name the same engine.
                let actual = fetched.at(&pointer);
                assert!(
                    actual.as_str().is_some_and(|reported| reported
                        .eq_ignore_ascii_case(expected.as_str().unwrap_or_default())),
                    "case {case:?}: GetApplication{pointer} expected the engine \
                     {expected:?} named, got {actual:?}"
                );
                continue;
            }
            assert_eq!(
                &fetched.at(&pointer),
                expected,
                "case {case:?}: GetApplication{pointer} expected {expected:?}, got {:?}",
                fetched.at(&pointer)
            );
        }

        // `AutoStartConfig$enabled` ("Defaults to true"),
        // `AutoStopConfig$enabled` ("Defaults to true") and
        // `AutoStopConfig$idleTimeoutMinutes` ("Defaults to 15 minutes") are reported
        // with the values the request gave, or with those documented defaults.
        let expected_auto_start = members
            .get("autoStartConfiguration")
            .and_then(|config| config.get("enabled"))
            .cloned()
            .unwrap_or(json!(true));
        assert_eq!(
            fetched.at("/application/autoStartConfiguration/enabled"),
            expected_auto_start,
            "case {case:?}: autoStartConfiguration.enabled expected {expected_auto_start:?}, got {:?}",
            fetched.at("/application/autoStartConfiguration/enabled")
        );
        let expected_auto_stop = members
            .get("autoStopConfiguration")
            .and_then(|config| config.get("enabled"))
            .cloned()
            .unwrap_or(json!(true));
        assert_eq!(
            fetched.at("/application/autoStopConfiguration/enabled"),
            expected_auto_stop,
            "case {case:?}: autoStopConfiguration.enabled expected {expected_auto_stop:?}, got {:?}",
            fetched.at("/application/autoStopConfiguration/enabled")
        );
        let expected_idle = members
            .get("autoStopConfiguration")
            .and_then(|config| config.get("idleTimeoutMinutes"))
            .cloned()
            .unwrap_or(json!(15));
        assert_eq!(
            fetched.at("/application/autoStopConfiguration/idleTimeoutMinutes"),
            expected_idle,
            "case {case:?}: autoStopConfiguration.idleTimeoutMinutes expected {expected_idle:?}, got {:?}",
            fetched.at("/application/autoStopConfiguration/idleTimeoutMinutes")
        );

        // And the summary `ListApplications` serves projects the same record.
        let listed = list_applications(&router, "").await;
        let summary = listed
            .body
            .get("applications")
            .and_then(Value::as_array)
            .expect("ListApplications returns an applications list")
            .iter()
            .find(|entry| entry["id"] == json!(id))
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the application must be listed"));
        for (summary_member, application_member) in [
            ("arn", "arn"),
            ("name", "name"),
            ("releaseLabel", "releaseLabel"),
            ("type", "type"),
            ("state", "state"),
            ("architecture", "architecture"),
            ("createdAt", "createdAt"),
            ("updatedAt", "updatedAt"),
        ] {
            let expected = fetched.at(&format!("/application/{application_member}"));
            let actual = summary
                .get(summary_member)
                .cloned()
                .unwrap_or(Value::Null);
            assert_eq!(
                actual, expected,
                "case {case:?}: the summary's {summary_member} must equal the \
                 application's {application_member} ({expected:?}), got {actual:?}"
            );
        }
    }
}

/// A created application is not usable the instant the create returns: it reports the
/// transient state to a caller acting straight away, and the clock owes it the settled
/// one without any further call. The same holds for a start and for a stop.
#[tokio::test]
async fn every_lifecycle_call_reports_its_transient_state_until_the_clock_settles_it() {
    let router = emulator();

    // (case, the transient state, the settled state, how to start the transition)
    let started_at = Instant::now();
    let created = create_application(&router, minimal_create("tok-transient")).await;
    let id = created
        .at("/applicationId")
        .as_str()
        .unwrap_or_default()
        .to_string();
    let immediately = get_application(&router, &id).await;
    if started_at.elapsed() < LIFECYCLE_TRANSITION {
        assert_eq!(
            immediately.at("/application/state"),
            json!("CREATING"),
            "case create: a caller acting inside the transition span sees CREATING, got {:?}",
            immediately.at("/application/state")
        );
    }
    wait_for_transition();
    assert_eq!(
        get_application(&router, &id).await.at("/application/state"),
        json!("CREATED"),
        "case create: the clock owes the application CREATED"
    );

    let started_at = Instant::now();
    start_application(&router, &id).await;
    let immediately = get_application(&router, &id).await;
    if started_at.elapsed() < LIFECYCLE_TRANSITION {
        assert_eq!(
            immediately.at("/application/state"),
            json!("STARTING"),
            "case start: a caller acting inside the transition span sees STARTING, got {:?}",
            immediately.at("/application/state")
        );
    }
    wait_for_transition();
    assert_eq!(
        get_application(&router, &id).await.at("/application/state"),
        json!("STARTED"),
        "case start: the clock owes the application STARTED"
    );

    let started_at = Instant::now();
    stop_application(&router, &id).await;
    let immediately = get_application(&router, &id).await;
    if started_at.elapsed() < LIFECYCLE_TRANSITION {
        assert_eq!(
            immediately.at("/application/state"),
            json!("STOPPING"),
            "case stop: a caller acting inside the transition span sees STOPPING, got {:?}",
            immediately.at("/application/state")
        );
    }
    wait_for_transition();
    assert_eq!(
        get_application(&router, &id).await.at("/application/state"),
        json!("STOPPED"),
        "case stop: the clock owes the application STOPPED"
    );
}

/// `clientToken` is `@idempotencyToken`: the same token names the same resource, a
/// different one names a new resource. Asserted for both operations that carry one.
#[tokio::test]
async fn the_same_client_token_names_the_same_resource() {
    let router = emulator();

    // Applications: repeating a token must not mint a second application.
    let tokens = ["tok-a", "tok-b", "tok-a", "tok.c_1-2", "tok-b"];
    let mut minted: Vec<String> = Vec::new();
    for token in tokens {
        let answer = create_application(&router, minimal_create(token)).await;
        assert_eq!(
            answer.status, 200,
            "case {token:?}: CreateApplication expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        minted.push(
            answer
                .at("/applicationId")
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }
    assert_eq!(
        minted[0], minted[2],
        "case tok-a: the repeated token must name the first application, got {:?} then {:?}",
        minted[0], minted[2]
    );
    assert_eq!(
        minted[1], minted[4],
        "case tok-b: the repeated token must name the first application, got {:?} then {:?}",
        minted[1], minted[4]
    );
    let distinct: BTreeSet<&String> = minted.iter().collect();
    assert_eq!(
        distinct.len(),
        3,
        "three distinct tokens must have minted three applications, got {minted:?}"
    );
    let listed = list_applications(&router, "").await;
    assert_eq!(
        listed
            .body
            .get("applications")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(3),
        "the store must hold one application per distinct token, got {:?}",
        listed.body
    );

    // Job runs: the same, scoped to the application the run belongs to.
    let application_id = started_application(&router, "tok-jobs").await;
    let job_tokens = ["job-a", "job-b", "job-a"];
    let mut job_ids: Vec<String> = Vec::new();
    for token in job_tokens {
        let answer = start_job_run(&router, &application_id, minimal_job_run(token)).await;
        assert_eq!(
            answer.status, 200,
            "case {token:?}: StartJobRun expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        job_ids.push(
            answer
                .at("/jobRunId")
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }
    assert_eq!(
        job_ids[0], job_ids[2],
        "case job-a: the repeated token must name the first job run, got {:?} then {:?}",
        job_ids[0], job_ids[2]
    );
    assert_ne!(
        job_ids[0], job_ids[1],
        "case job-b: a different token must name a new job run, got {job_ids:?}"
    );
    let runs = list_job_runs(&router, &application_id, "").await;
    assert_eq!(
        runs.body
            .get("jobRuns")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(2),
        "the application must hold one job run per distinct token, got {:?}",
        runs.body
    );
}

/// An id that names nothing is refused, with the status and code the model declares for
/// that operation — never a 200 with an empty body and never a 501.
#[tokio::test]
async fn an_id_that_names_nothing_is_refused() {
    let router = emulator();
    let application_id = started_application(&router, "tok-notfound").await;
    let submitted = start_job_run(&router, &application_id, minimal_job_run("job-1")).await;
    let job_run_id = submitted
        .at("/jobRunId")
        .as_str()
        .unwrap_or_default()
        .to_string();

    // Every operation whose model declares `ResourceNotFoundException` (HTTP 404)
    // answers it for an unknown id.
    let unknown = "doesnotexist";
    for (case, method, uri) in [
        (
            "GetApplication",
            "GET",
            format!("/applications/{unknown}"),
        ),
        (
            "StartApplication",
            "POST",
            format!("/applications/{unknown}/start"),
        ),
        (
            "StopApplication",
            "POST",
            format!("/applications/{unknown}/stop"),
        ),
        (
            "DeleteApplication",
            "DELETE",
            format!("/applications/{unknown}"),
        ),
        (
            "GetJobRun on an unknown application",
            "GET",
            format!("/applications/{unknown}/jobruns/{job_run_id}"),
        ),
        (
            "GetJobRun on an unknown job run",
            "GET",
            format!("/applications/{application_id}/jobruns/{unknown}"),
        ),
        (
            "CancelJobRun on an unknown job run",
            "DELETE",
            format!("/applications/{application_id}/jobruns/{unknown}"),
        ),
    ] {
        let answer = call(&router, method, &uri, None).await;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected 404, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            answer.body
        );
    }

    // `StartJobRun` against an unknown application is the same refusal.
    let orphan = start_job_run(&router, unknown, minimal_job_run("job-orphan")).await;
    assert_eq!(
        orphan.status, 404,
        "case StartJobRun on an unknown application: expected 404, got {} with {:?}",
        orphan.status, orphan.body
    );
    assert_eq!(
        orphan.error_code(),
        "ResourceNotFoundException",
        "case StartJobRun on an unknown application: expected ResourceNotFoundException, got {:?}",
        orphan.body
    );

    // `ListJobRuns` declares no `ResourceNotFoundException`
    // (`smithy_query(query="operation",operation="ListJobRuns")` lists only
    // `InternalServerException` and `ValidationException`), so its refusal is the
    // modelled 400.
    let listed = list_job_runs(&router, unknown, "").await;
    assert_eq!(
        listed.status, 400,
        "case ListJobRuns on an unknown application: expected 400, got {} with {:?}",
        listed.status, listed.body
    );
    assert_eq!(
        listed.error_code(),
        "ValidationException",
        "case ListJobRuns on an unknown application: expected ValidationException, got {:?}",
        listed.body
    );
}

/// `DeleteApplication` documents its precondition — "An application has to be in a
/// stopped or created state in order to be deleted" — judged against the state the
/// application has SETTLED on: it admits a transition on its way to `CREATED` or
/// `STOPPED` and refuses one on its way to `STARTED`. An accepted delete leaves the
/// application readable in the terminal `TERMINATED` state.
#[tokio::test]
async fn delete_application_admits_the_states_that_settle_on_the_ones_it_documents() {
    // One row per `ApplicationState` a client can put an application into: the state it
    // reports, the state it settles on and the status the delete must answer. Source:
    // the `ApplicationState` enum (`CREATING`, `CREATED`, `STARTING`, `STARTED`,
    // `STOPPING`, `STOPPED`, `TERMINATED`), every value of which is a row here.
    let cases = [
        ("CREATING", "CREATED", 200),
        ("CREATED", "CREATED", 200),
        ("STARTING", "STARTED", 400),
        ("STARTED", "STARTED", 400),
        ("STOPPING", "STOPPED", 200),
        ("STOPPED", "STOPPED", 200),
        ("TERMINATED", "TERMINATED", 400),
    ];

    for (state, settles_on, expected_status) in cases {
        let router = emulator();
        let created = create_application(&router, minimal_create("tok-delete")).await;
        let id = created
            .at("/applicationId")
            .as_str()
            .unwrap_or_default()
            .to_string();
        match state {
            "CREATING" => {}
            "CREATED" => wait_for_transition(),
            "STARTING" => {
                wait_for_transition();
                start_application(&router, &id).await;
            }
            "STARTED" => {
                wait_for_transition();
                start_application(&router, &id).await;
                wait_for_transition();
            }
            "STOPPING" => {
                wait_for_transition();
                stop_application(&router, &id).await;
            }
            "STOPPED" => {
                wait_for_transition();
                stop_application(&router, &id).await;
                wait_for_transition();
            }
            "TERMINATED" => {
                wait_for_transition();
                delete_application(&router, &id).await;
            }
            other => panic!("unhandled state {other}"),
        }

        let observed = get_application(&router, &id).await;
        assert_eq!(
            observed.at("/application/state"),
            json!(state),
            "case {state:?}: the application must be in that state before the delete, got {:?}",
            observed.at("/application/state")
        );

        let deleted = delete_application(&router, &id).await;
        assert_eq!(
            deleted.status, expected_status,
            "case {state:?}: DeleteApplication on an application settling on \
             {settles_on:?} expected {expected_status}, got {} with {:?}",
            deleted.status, deleted.body
        );
        let after = get_application(&router, &id).await;
        assert_eq!(
            after.status, 200,
            "case {state:?}: the application is still readable after the delete, got {} \
             with {:?}",
            after.status, after.body
        );
        if expected_status == 400 {
            assert_eq!(
                deleted.error_code(),
                "ValidationException",
                "case {state:?}: expected ValidationException, got {:?}",
                deleted.body
            );
            assert_eq!(
                after.at("/application/state"),
                json!(state),
                "case {state:?}: a refused delete must leave the state alone, got {:?}",
                after.at("/application/state")
            );
        } else {
            assert_eq!(
                after.at("/application/state"),
                json!("TERMINATED"),
                "case {state:?}: an accepted delete must report TERMINATED, got {:?}",
                after.at("/application/state")
            );
        }
    }
}

/// `StopApplication` documents the precondition the model cannot express: "All
/// scheduled and running jobs must be completed or cancelled before stopping an
/// application."
#[tokio::test]
async fn stop_application_requires_its_job_runs_to_have_finished() {
    let router = emulator();
    let application_id = started_application(&router, "tok-stop").await;
    start_job_run(&router, &application_id, minimal_job_run("job-1")).await;

    let refused = stop_application(&router, &application_id).await;
    assert_eq!(
        refused.status, 400,
        "case an unfinished job run: StopApplication expected 400, got {} with {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code(),
        "ValidationException",
        "case an unfinished job run: expected ValidationException, got {:?}",
        refused.body
    );
    assert_eq!(
        get_application(&router, &application_id)
            .await
            .at("/application/state"),
        json!("STARTED"),
        "case an unfinished job run: the refused stop must leave the application STARTED"
    );

    // Once the job run has finished, the stop is accepted.
    wait_for_job_run_to_settle();
    let accepted = stop_application(&router, &application_id).await;
    assert_eq!(
        accepted.status, 200,
        "case every job run finished: StopApplication expected 200, got {} with {:?}",
        accepted.status, accepted.body
    );
}

/// A job run's terminal state is what RUNNING the work its driver submitted came to. This
/// emulator runs the Spark submit of a Python application in Amazon S3, so a submit whose
/// entry point the object store holds nothing at ends in the modelled failure state carrying
/// what the STORE answered — and never in a success nothing computed.
///
/// A run that terminated also reports what it TOOK: the instant it moved to `RUNNING`, the
/// instant it was terminated, the seconds between them, the total time it spent `QUEUED`, and
/// the resources its own worker held over the span it executed for.
#[tokio::test]
async fn a_job_run_whose_application_cannot_be_downloaded_ends_failed() {
    let router = emulator();
    let application_id = started_application(&router, "tok-exec").await;
    let submitted = start_job_run(&router, &application_id, minimal_job_run("job-1")).await;
    let job_run_id = submitted
        .at("/jobRunId")
        .as_str()
        .unwrap_or_default()
        .to_string();

    let immediately = get_job_run(&router, &application_id, &job_run_id).await;
    assert_eq!(
        immediately.at("/jobRun/state"),
        json!("SUBMITTED"),
        "a job run is SUBMITTED when it is accepted, got {:?}",
        immediately.at("/jobRun/state")
    );
    assert_eq!(
        immediately.at("/jobRun/totalResourceUtilization"),
        Value::Null,
        "nothing has executed yet, so there is no resource utilisation, got {:?}",
        immediately.at("/jobRun/totalResourceUtilization")
    );

    let settled = settled_job_run(&router, &application_id, &job_run_id).await;
    assert_eq!(
        settled.at("/jobRun/state"),
        json!("FAILED"),
        "a job run whose application could not be downloaded must end FAILED, got {:?}",
        settled.at("/jobRun/state")
    );
    let details = settled.at("/jobRun/stateDetails");
    let details = details.as_str().unwrap_or_default();
    assert!(
        details.contains(JOB_RUN_UNREADABLE_LEAD),
        "the failure must say the application could not be downloaded, got {details:?}"
    );
    assert!(
        !details.contains("app.py"),
        "`SparkSubmit$entryPoint` is @sensitive, so no state detail echoes it, got \
         {details:?}"
    );
    assert_ne!(
        settled.at("/jobRun/endedAt"),
        Value::Null,
        "a finished job run carries the instant it ended, got {:?}",
        settled.at("/jobRun/endedAt")
    );
    assert_ne!(
        settled.at("/jobRun/startedAt"),
        Value::Null,
        "a run whose work was executed carries the instant it moved to RUNNING, got {:?}",
        settled.at("/jobRun/startedAt")
    );
    let duration = settled.at("/jobRun/totalExecutionDurationSeconds");
    assert!(
        duration.as_i64().is_some_and(|seconds| seconds >= 1),
        "a run that executed reports the seconds it took, got {duration:?}"
    );
    // "The total time for a job in the QUEUED state in milliseconds"
    // (`JobRun$queuedDurationMilliseconds`): the run passed through that state, so the span
    // it spent there is reported.
    let queued = settled.at("/jobRun/queuedDurationMilliseconds");
    assert!(
        queued.as_i64().is_some_and(|milliseconds| milliseconds >= 0),
        "a run that passed through QUEUED reports the time it spent there, got {queued:?}"
    );
    // ... and what the span it executed for consumed, in both utilisation structures: the
    // aggregate, and the billed figures under the documented one-minute minimum and the 20 GB
    // of per-worker storage that is not billed.
    for member in ["vCPUHour", "memoryGBHour", "storageGBHour"] {
        let used = settled.at(&format!("/jobRun/totalResourceUtilization/{member}"));
        assert!(
            used.as_f64().is_some_and(|value| value > 0.0),
            "a terminated run reports the {member} it used, got {used:?}"
        );
    }
    let billed_v_cpu = settled.at("/jobRun/billedResourceUtilization/vCPUHour");
    let used_v_cpu = settled.at("/jobRun/totalResourceUtilization/vCPUHour");
    assert!(
        billed_v_cpu.as_f64().is_some_and(|billed| {
            used_v_cpu.as_f64().is_some_and(|used| billed >= used)
        }),
        "the billed vCPU-hours cover at least the minimum usage, got {billed_v_cpu:?} against \
         {used_v_cpu:?}"
    );
    let billed_storage = settled.at("/jobRun/billedResourceUtilization/storageGBHour");
    assert_eq!(
        billed_storage.as_f64(),
        Some(0.0),
        "a worker at the storage size that is not billed is billed none of it, got \
         {billed_storage:?}"
    );
}

/// `JobRun$executionTimeoutMinutes` documents its default: "If you didn't specify a
/// timeout, this value defaults to 720 minutes." Any value the caller does give is kept.
#[tokio::test]
async fn a_job_runs_execution_timeout_is_the_one_given_or_the_documented_default() {
    // The rows span the modelled `@range(min: 0, max: 1000000)`: both published bounds,
    // a value inside it, and the absent member that takes the default.
    let cases: Vec<(&str, Option<i64>, i64)> = vec![
        ("absent", None, DEFAULT_EXECUTION_TIMEOUT_MINUTES),
        ("the range's minimum", Some(0), 0),
        ("a value inside the range", Some(45), 45),
        ("the range's maximum", Some(1_000_000), 1_000_000),
    ];

    let router = emulator();
    let application_id = started_application(&router, "tok-timeout").await;
    for (index, (case, given, expected)) in cases.into_iter().enumerate() {
        let mut request = minimal_job_run(&format!("job-timeout-{index}"));
        if let Some(minutes) = given {
            request["executionTimeoutMinutes"] = json!(minutes);
        }
        let submitted = start_job_run(&router, &application_id, request).await;
        assert_eq!(
            submitted.status, 200,
            "case {case:?}: StartJobRun expected 200, got {} with {:?}",
            submitted.status, submitted.body
        );
        let job_run_id = submitted
            .at("/jobRunId")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let run = get_job_run(&router, &application_id, &job_run_id).await;
        assert_eq!(
            run.at("/jobRun/executionTimeoutMinutes"),
            json!(expected),
            "case {case:?}: executionTimeoutMinutes expected {expected}, got {:?}",
            run.at("/jobRun/executionTimeoutMinutes")
        );
    }
}

/// `JobRun$jobDriver` is required on everything that reports a job run, so a submit
/// naming no driver is refused rather than stored.
#[tokio::test]
async fn start_job_run_without_a_job_driver_is_refused() {
    let router = emulator();
    let application_id = started_application(&router, "tok-nodriver").await;

    let refused = start_job_run(
        &router,
        &application_id,
        json!({ "clientToken": "job-nodriver", "executionRoleArn": EXECUTION_ROLE }),
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "case no jobDriver: expected 400, got {} with {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code(),
        "ValidationException",
        "case no jobDriver: expected ValidationException, got {:?}",
        refused.body
    );
    let runs = list_job_runs(&router, &application_id, "").await;
    assert_eq!(
        runs.at("/jobRuns"),
        json!([]),
        "case no jobDriver: the refused submit must store nothing, got {:?}",
        runs.body
    );

    // Both members of the `JobDriver` union are accepted.
    for (case, driver) in [
        ("sparkSubmit", spark_submit_driver("s3://bucket/app.py")),
        (
            "hive",
            json!({ "hive": { "query": "SELECT 1", "initQueryFile": "s3://bucket/init.q" } }),
        ),
    ] {
        let accepted = start_job_run(
            &router,
            &application_id,
            json!({
                "clientToken": format!("job-driver-{case}"),
                "executionRoleArn": EXECUTION_ROLE,
                "jobDriver": driver,
            }),
        )
        .await;
        assert_eq!(
            accepted.status, 200,
            "case {case:?}: StartJobRun expected 200, got {} with {:?}",
            accepted.status, accepted.body
        );
        let job_run_id = accepted
            .at("/jobRunId")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let run = get_job_run(&router, &application_id, &job_run_id).await;
        assert_eq!(
            run.at(&format!("/jobRun/jobDriver/{case}")),
            driver[case],
            "case {case:?}: the stored driver must read back unchanged, got {:?}",
            run.at("/jobRun/jobDriver")
        );
    }
}

/// `AutoStartConfig` documents that an application "automatically start[s] on job
/// submission", defaulting to true; an application configured otherwise refuses a
/// submit while it is not running.
#[tokio::test]
async fn a_job_submission_starts_the_application_only_when_auto_start_is_enabled() {
    let cases = [
        ("auto start left to its documented default", None, 200),
        ("auto start enabled explicitly", Some(true), 200),
        ("auto start disabled", Some(false), 400),
    ];

    for (case, enabled, expected_status) in cases {
        let router = emulator();
        let mut request = minimal_create("tok-autostart");
        if let Some(enabled) = enabled {
            request["autoStartConfiguration"] = json!({ "enabled": enabled });
        }
        create_application(&router, request).await;
        wait_for_transition();
        let id = list_applications(&router, "").await.at("/applications/0/id");
        let id = id.as_str().unwrap_or_default().to_string();

        let submitted = start_job_run(&router, &id, minimal_job_run("job-1")).await;
        assert_eq!(
            submitted.status, expected_status,
            "case {case:?}: StartJobRun expected {expected_status}, got {} with {:?}",
            submitted.status, submitted.body
        );
        if expected_status == 200 {
            assert_eq!(
                get_application(&router, &id).await.at("/application/state"),
                json!("STARTING"),
                "case {case:?}: the submit must have started the application"
            );
        } else {
            assert_eq!(
                submitted.error_code(),
                "ValidationException",
                "case {case:?}: expected ValidationException, got {:?}",
                submitted.body
            );
            assert_eq!(
                get_application(&router, &id).await.at("/application/state"),
                json!("CREATED"),
                "case {case:?}: a refused submit must leave the application as it was"
            );
        }
    }
}

/// `CancelJobRun` moves a running job run to `CANCELLING` and the clock owes it
/// `CANCELLED`; a repeat cancel of a run that is already `CANCELLED` is the idempotent
/// success (the refusal of a run that finished some other way is asserted in
/// `idempotency_and_idle_lifecycle`). The `shutdownGracePeriodInSeconds` the caller asks
/// for is the wait that applies.
#[tokio::test]
async fn cancel_job_run_moves_a_live_run_and_refuses_a_finished_one() {
    let router = emulator();
    let application_id = started_application(&router, "tok-cancel").await;

    let submitted = start_job_run(&router, &application_id, minimal_job_run("job-live")).await;
    let job_run_id = submitted
        .at("/jobRunId")
        .as_str()
        .unwrap_or_default()
        .to_string();
    let cancelled = cancel_job_run(&router, &application_id, &job_run_id, "").await;
    assert_eq!(
        cancelled.status, 200,
        "case a live run: CancelJobRun expected 200, got {} with {:?}",
        cancelled.status, cancelled.body
    );
    assert_eq!(
        get_job_run(&router, &application_id, &job_run_id)
            .await
            .at("/jobRun/state"),
        json!("CANCELLING"),
        "case a live run: the run reports CANCELLING straight after the cancel"
    );
    wait_for_transition();
    assert_eq!(
        get_job_run(&router, &application_id, &job_run_id)
            .await
            .at("/jobRun/state"),
        json!("CANCELLED"),
        "case a live run: the clock owes the run CANCELLED"
    );

    // `@idempotent`: cancelling a run that is ALREADY cancelled is the success the
    // operation promises, so the repeat answers 200 with the same ids and moves nothing.
    let again = cancel_job_run(&router, &application_id, &job_run_id, "").await;
    assert_eq!(
        again.status, 200,
        "case an already-cancelled run: CancelJobRun expected 200, got {} with {:?}",
        again.status, again.body
    );
    assert_eq!(
        (again.at("/applicationId"), again.at("/jobRunId")),
        (json!(application_id), json!(job_run_id)),
        "case an already-cancelled run: the repeat must echo the same ids, got {:?}",
        again.body
    );
    assert_eq!(
        get_job_run(&router, &application_id, &job_run_id)
            .await
            .at("/jobRun/state"),
        json!("CANCELLED"),
        "case an already-cancelled run: the repeat must not move the state"
    );

    // `shutdownGracePeriodInSeconds` is "the duration in seconds to wait before
    // forcefully terminating the job after cancellation is requested", so a grace
    // period longer than the emulator's own span keeps the run CANCELLING past it.
    let slow = start_job_run(&router, &application_id, minimal_job_run("job-grace")).await;
    let slow_id = slow.at("/jobRunId").as_str().unwrap_or_default().to_string();
    let grace = cancel_job_run(
        &router,
        &application_id,
        &slow_id,
        "?shutdownGracePeriodInSeconds=120",
    )
    .await;
    assert_eq!(
        grace.status, 200,
        "case a long grace period: CancelJobRun expected 200, got {} with {:?}",
        grace.status, grace.body
    );
    wait_for_transition();
    assert_eq!(
        get_job_run(&router, &application_id, &slow_id)
            .await
            .at("/jobRun/state"),
        json!("CANCELLING"),
        "case a long grace period: the run is still CANCELLING inside the wait the caller asked for"
    );
}

/// Both list operations page over a deterministic order: concatenating the pages
/// yields every stored record exactly once, whatever the page size.
#[tokio::test]
async fn paging_a_list_neither_skips_nor_duplicates_a_record() {
    let router = emulator();

    // Nine applications, so several page sizes divide the set unevenly.
    let mut expected_applications: Vec<String> = Vec::new();
    for index in 0..9 {
        let answer =
            create_application(&router, minimal_create(&format!("tok-page-{index}"))).await;
        expected_applications.push(
            answer
                .at("/applicationId")
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }
    expected_applications.sort();

    // The rows span the modelled `@range(min: 1, max: 50)`: both published bounds, two
    // sizes that divide the set unevenly, one that divides it exactly, and the absent
    // member that serves the whole page limit.
    for (case, query) in [
        ("the range's minimum", "?maxResults=1".to_string()),
        ("a page smaller than the set", "?maxResults=2".to_string()),
        ("a page dividing the set exactly", "?maxResults=3".to_string()),
        ("a page larger than the set", "?maxResults=20".to_string()),
        (
            "the range's maximum",
            format!("?maxResults={PAGE_LIMIT}"),
        ),
        ("no maxResults", String::new()),
    ] {
        let mut seen: Vec<String> = Vec::new();
        let mut next: Option<String> = None;
        for _ in 0..32 {
            let uri = match next.as_deref() {
                None => query.clone(),
                Some(token) => {
                    if query.is_empty() {
                        format!("?nextToken={token}")
                    } else {
                        format!("{query}&nextToken={token}")
                    }
                }
            };
            let page = list_applications(&router, &uri).await;
            assert_eq!(
                page.status, 200,
                "case {case:?}: ListApplications expected 200, got {} with {:?}",
                page.status, page.body
            );
            for entry in page
                .body
                .get("applications")
                .and_then(Value::as_array)
                .unwrap_or(&Vec::new())
            {
                seen.push(entry["id"].as_str().unwrap_or_default().to_string());
            }
            match page.at("/nextToken").as_str() {
                Some(token) => {
                    assert!(
                        matches_next_token(token),
                        "case {case:?}: nextToken must match ^[A-Za-z0-9_=-]+$, got {token:?}"
                    );
                    next = Some(token.to_string());
                }
                None => {
                    next = None;
                    break;
                }
            }
        }
        assert!(
            next.is_none(),
            "case {case:?}: paging must terminate, got a cursor after 32 pages"
        );
        assert_eq!(
            seen, expected_applications,
            "case {case:?}: the pages must concatenate to every application exactly once, \
             got {seen:?}"
        );
    }

    // A token this service did not mint is refused, not read as a resume point.
    for (case, token) in [
        ("not a cursor at all", "nonsense"),
        ("the right prefix with a bad body", "v1_zz"),
    ] {
        let answer = list_applications(&router, &format!("?nextToken={token}")).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected 400, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {:?}",
            answer.body
        );
    }

    // The same for job runs, scoped to one application.
    let application_id = started_application(&router, "tok-page-runs").await;
    let mut expected_runs: Vec<String> = Vec::new();
    for index in 0..5 {
        let answer = start_job_run(
            &router,
            &application_id,
            minimal_job_run(&format!("job-page-{index}")),
        )
        .await;
        expected_runs.push(
            answer
                .at("/jobRunId")
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }
    expected_runs.sort();
    for (case, page_size) in [("one at a time", 1), ("two at a time", 2), ("all at once", 50)] {
        let mut seen: Vec<String> = Vec::new();
        let mut next: Option<String> = None;
        for _ in 0..32 {
            let uri = match next.as_deref() {
                None => format!("?maxResults={page_size}"),
                Some(token) => format!("?maxResults={page_size}&nextToken={token}"),
            };
            let page = list_job_runs(&router, &application_id, &uri).await;
            for entry in page
                .body
                .get("jobRuns")
                .and_then(Value::as_array)
                .unwrap_or(&Vec::new())
            {
                seen.push(entry["id"].as_str().unwrap_or_default().to_string());
            }
            match page.at("/nextToken").as_str() {
                Some(token) => next = Some(token.to_string()),
                None => {
                    next = None;
                    break;
                }
            }
        }
        assert!(
            next.is_none(),
            "case {case:?}: paging job runs must terminate"
        );
        assert_eq!(
            seen, expected_runs,
            "case {case:?}: the pages must concatenate to every job run exactly once, got {seen:?}"
        );
    }
}

/// A list reports only the records that pass its filters, and a job run list only the
/// runs of the application it names.
#[tokio::test]
async fn a_list_reports_only_the_records_its_filters_admit() {
    let router = emulator();

    // Two applications in different states, so `states` has something to separate.
    let created_only = created_application(&router, "tok-filter-created").await;
    let running = started_application(&router, "tok-filter-started").await;

    // Rows spanning the `ApplicationStateSet` filter: one state, the other state, both,
    // a state nothing is in, and the absent filter. Source: the `ApplicationState`
    // enum.
    let cases: Vec<(&str, &str, Vec<&String>)> = vec![
        ("one state", "?states=CREATED", vec![&created_only]),
        ("the other state", "?states=STARTED", vec![&running]),
        (
            "both states",
            "?states=CREATED&states=STARTED",
            vec![&created_only, &running],
        ),
        ("a state nothing is in", "?states=TERMINATED", vec![]),
        ("no filter", "", vec![&created_only, &running]),
    ];
    for (case, query, expected) in cases {
        let mut expected: Vec<String> = expected.into_iter().cloned().collect();
        expected.sort();
        let page = list_applications(&router, query).await;
        assert_eq!(
            page.status, 200,
            "case {case:?}: ListApplications expected 200, got {} with {:?}",
            page.status, page.body
        );
        let mut seen: Vec<String> = page
            .body
            .get("applications")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
            .iter()
            .map(|entry| entry["id"].as_str().unwrap_or_default().to_string())
            .collect();
        seen.sort();
        assert_eq!(
            seen, expected,
            "case {case:?}: expected {expected:?}, got {seen:?}"
        );
    }

    // Job runs: one BATCH and one STREAMING run on the started application, and one run
    // on a second application that must never appear in the first one's list. The
    // second application is created FIRST, so the runs below are not waiting on it.
    let other_application = started_application(&router, "tok-filter-other").await;
    let other_run = start_job_run(&router, &other_application, minimal_job_run("job-other"))
        .await
        .at("/jobRunId")
        .as_str()
        .unwrap_or_default()
        .to_string();

    let batch = start_job_run(
        &router,
        &running,
        json!({
            "clientToken": "job-batch",
            "executionRoleArn": EXECUTION_ROLE,
            "jobDriver": spark_submit_driver("s3://bucket/batch.py"),
            "mode": "BATCH",
        }),
    )
    .await
    .at("/jobRunId")
    .as_str()
    .unwrap_or_default()
    .to_string();
    let streaming = start_job_run(
        &router,
        &running,
        json!({
            "clientToken": "job-streaming",
            "executionRoleArn": EXECUTION_ROLE,
            "jobDriver": spark_submit_driver("s3://bucket/stream.py"),
            "mode": "STREAMING",
        }),
    )
    .await
    .at("/jobRunId")
    .as_str()
    .unwrap_or_default()
    .to_string();

    // Settle both runs into two DIFFERENT terminal states, so the `states` rows below
    // separate them without depending on when this test runs: the streaming run is
    // cancelled, the batch run is left to the clock.
    cancel_job_run(&router, &running, &streaming, "").await;
    wait_for_job_run_to_settle();

    // Rows spanning the `mode` filter (the whole `JobRunMode` enum: `BATCH`,
    // `STREAMING`) and the `states` filter (values from the `JobRunState` enum).
    let run_cases: Vec<(&str, &str, Vec<&String>)> = vec![
        ("mode BATCH", "?mode=BATCH", vec![&batch]),
        ("mode STREAMING", "?mode=STREAMING", vec![&streaming]),
        ("state FAILED", "?states=FAILED", vec![&batch]),
        ("state CANCELLED", "?states=CANCELLED", vec![&streaming]),
        (
            "both states",
            "?states=FAILED&states=CANCELLED",
            vec![&batch, &streaming],
        ),
        ("a state nothing is in", "?states=SUCCESS", vec![]),
        ("no filter", "", vec![&batch, &streaming]),
    ];
    for (case, query, expected) in run_cases {
        let mut expected: Vec<String> = expected.into_iter().cloned().collect();
        expected.sort();
        let page = list_job_runs(&router, &running, query).await;
        assert_eq!(
            page.status, 200,
            "case {case:?}: ListJobRuns expected 200, got {} with {:?}",
            page.status, page.body
        );
        let mut seen: Vec<String> = page
            .body
            .get("jobRuns")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
            .iter()
            .map(|entry| entry["id"].as_str().unwrap_or_default().to_string())
            .collect();
        seen.sort();
        assert_eq!(
            seen, expected,
            "case {case:?}: expected {expected:?}, got {seen:?}"
        );
        assert!(
            !seen.contains(&other_run) || expected.contains(&other_run),
            "case {case:?}: another application's job run must never be listed, got {seen:?}"
        );
    }

    // `createdAtAfter` / `createdAtBefore` bound the creation instant.
    let created_at = get_job_run(&router, &running, &batch)
        .await
        .at("/jobRun/createdAt")
        .as_f64()
        .expect("createdAt is an epoch-seconds number");
    for (case, query, expect_present) in [
        (
            "a lower bound before the run",
            format!("?createdAtAfter={}", urlencode(&rfc3339(created_at - 60.0))),
            true,
        ),
        (
            "a lower bound after the run",
            format!("?createdAtAfter={}", urlencode(&rfc3339(created_at + 60.0))),
            false,
        ),
        (
            "an upper bound after the run",
            format!(
                "?createdAtBefore={}",
                urlencode(&rfc3339(created_at + 60.0))
            ),
            true,
        ),
        (
            "an upper bound before the run",
            format!(
                "?createdAtBefore={}",
                urlencode(&rfc3339(created_at - 60.0))
            ),
            false,
        ),
    ] {
        let page = list_job_runs(&router, &running, &query).await;
        assert_eq!(
            page.status, 200,
            "case {case:?}: ListJobRuns expected 200, got {} with {:?}",
            page.status, page.body
        );
        let seen: Vec<String> = page
            .body
            .get("jobRuns")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
            .iter()
            .map(|entry| entry["id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            seen.contains(&batch),
            expect_present,
            "case {case:?}: expected the run to be {}present, got {seen:?}",
            if expect_present { "" } else { "ab" }
        );
    }
}

/// Every operation this slice leaves unimplemented still answers HTTP 501, so a client
/// can tell "nobody has written it yet" from a wrong answer.
#[tokio::test]
async fn the_operations_this_slice_does_not_implement_still_answer_501() {
    let router = emulator();
    let application_id = started_application(&router, "tok-stubs").await;

    for (case, method, uri, body) in [
        (
            "GetResourceDashboard",
            "GET",
            format!("/applications/{application_id}/dashboard"),
            None,
        ),
    ] {
        let answer = call(&router, method, &uri, body).await;
        assert_eq!(
            answer.status, 501,
            "case {case:?}: an unimplemented operation must answer 501, got {} with {:?}",
            answer.status, answer.body
        );
    }
}

/// `Date`'s `httpQuery` binding is an RFC-3339 date-time (restJson1's default
/// timestamp format outside a JSON body), so a creation-time bound travels in that
/// form even though the same member comes back as epoch seconds in a response body.
pub(crate) fn rfc3339(epoch_seconds: f64) -> String {
    aws_smithy_types::DateTime::from_secs_f64(epoch_seconds)
        .fmt(aws_smithy_types::date_time::Format::DateTime)
        .expect("the instant formats as RFC-3339")
}

/// Percent-encode a path segment, so an ARN can travel in one.
pub(crate) fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

// --- what a SUBMITTED Spark program computes, and how long a run is observable in flight ---

/// The bucket the Spark rows below stage their input in and write their output to.
const SPARK_BUCKET: &str = "emr-serverless-spark-data";

/// The interval a client that waits for work polls at.
///
/// A client submits a job run and then looks for it — `GetJobRun`, or `ListJobRuns` filtered
/// on the non-terminal states — on the order of seconds; the recorded clients driving this
/// service look every ten. The state such a client is told the run is in must not depend on
/// how coarsely it looks, which is what the window a run is non-terminal for has to outlast.
const CLIENT_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// The non-terminal `JobRunState` values a client filters a listing on to find work in
/// flight. Source: `smithy_query(query="shape",shape="JobRunState")` — the enum's values
/// less the three terminal ones. Written out here rather than imported, so a wrong set in
/// `src/` cannot make this suite pass.
const IN_FLIGHT_STATES: &[&str] = &["SUBMITTED", "PENDING", "QUEUED", "SCHEDULED", "RUNNING"];

/// One path-style Amazon S3 call against the emulator this suite builds, answered as the
/// whole HTTP response: the body of an object is BYTES, which is what a columnar object has
/// to be read as.
pub(crate) async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Bytes,
) -> (u16, Bytes) {
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/s3/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        );
    if !body.is_empty() {
        builder = builder.header("content-type", "application/octet-stream");
    }
    let request = builder.body(body).expect("the test request is well formed");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.into_body())
}

/// Create [`SPARK_BUCKET`], so the paths the rows name resolve.
pub(crate) async fn given_spark_bucket(router: &MultiServiceRouter) {
    let configuration = format!(
        "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
    );
    let (status, body) = s3(
        router,
        "PUT",
        &format!("/{SPARK_BUCKET}"),
        Bytes::from(configuration),
    )
    .await;
    assert!(
        status == 200 || status == 409,
        "creating s3://{SPARK_BUCKET} must succeed or report it is already there, got \
         {status} with {:?}",
        String::from_utf8_lossy(&body)
    );
}

/// A key as it travels in a path-style Amazon S3 URI: each segment percent-encoded, with the
/// separators between them left as they are.
fn key_in_a_uri(key: &str) -> String {
    key.split('/').map(urlencode).collect::<Vec<String>>().join("/")
}

/// Store one object in [`SPARK_BUCKET`].
pub(crate) async fn put_object(router: &MultiServiceRouter, key: &str, body: Bytes) {
    let (status, answered) = s3(
        router,
        "PUT",
        &format!("/{SPARK_BUCKET}/{}", key_in_a_uri(key)),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "storing s3://{SPARK_BUCKET}/{key} must succeed, got {status} with {:?}",
        String::from_utf8_lossy(&answered)
    );
}

/// The bytes of one object of [`SPARK_BUCKET`].
pub(crate) async fn get_object(router: &MultiServiceRouter, key: &str) -> Bytes {
    let (status, body) = s3(
        router,
        "GET",
        &format!("/{SPARK_BUCKET}/{}", key_in_a_uri(key)),
        Bytes::new(),
    )
    .await;
    assert_eq!(
        status, 200,
        "reading s3://{SPARK_BUCKET}/{key} must succeed, got {status} with {:?}",
        String::from_utf8_lossy(&body)
    );
    body
}

/// The keys of every object under `prefix` in [`SPARK_BUCKET`], from `ListObjectsV2`.
pub(crate) async fn keys_under(router: &MultiServiceRouter, prefix: &str) -> Vec<String> {
    let (status, listing) = s3(
        router,
        "GET",
        &format!("/{SPARK_BUCKET}?list-type=2&prefix={}", urlencode(prefix)),
        Bytes::new(),
    )
    .await;
    let listing = String::from_utf8_lossy(&listing).to_string();
    assert_eq!(
        status, 200,
        "listing s3://{SPARK_BUCKET}/{prefix} must succeed, got {status} with {listing}"
    );
    listing
        .split("<Key>")
        .skip(1)
        .filter_map(|rest| rest.split_once("</Key>").map(|(key, _)| key.to_string()))
        .collect()
}

/// The program every Spark row submits: it reads its source, GROUPS it (a shuffle), writes
/// the result as Parquet — partitioned by the columns its fourth argument names — reads that
/// output back, and FAILS unless what it read is what it wrote. The counts it then reports
/// are written as its own output, so the suite reads them back out of the object store.
const ROUNDTRIP_PROGRAM: &str = r#"
import sys

from pyspark.sql import SparkSession
from pyspark.sql.functions import col, count, sum as summed

source, output, report, keys = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
names = [name for name in keys.split(",") if name]

spark = SparkSession.builder.appName("partitioned-roundtrip").getOrCreate()
frame = (
    spark.read.option("header", "true")
    .schema("region string, dt string, amount double, items bigint")
    .csv(source)
)
grouped = frame.groupBy("region", "dt").agg(
    summed("amount").alias("total"),
    summed("items").alias("items"),
    count("*").alias("rows"),
)
writer = grouped.write.mode("overwrite")
if names:
    writer = writer.partitionBy(*names)
writer.parquet(output)

read = spark.read.parquet(output)


# What a dataset holds, by group. A partition column is resolved from the DIRECTORY that
# names it, whose own type Spark infers, so a group is keyed by the TEXT of its key values
# while its measures are compared as the values they are.
def keyed(frame):
    held = {}
    for row in frame.collect():
        held["%s|%s" % (row["region"], row["dt"])] = (
            row["total"],
            row["items"],
            row["rows"],
        )
    return held


held = keyed(read)
wrote = keyed(grouped)
if held != wrote:
    raise ValueError("the parquet round trip read %r, having written %r" % (held, wrote))

one = read.filter(col("region") == 'eu').orderBy("dt").collect()
spark.createDataFrame(
    [
        {
            "groups": len(held),
            "eu": len(one),
            "columns": ",".join(read.columns),
            "types": ",".join("%s:%s" % pair for pair in read.dtypes),
        }
    ]
).coalesce(1).write.mode("overwrite").json(report)
"#;
/// One row of the input the program reads: a group key, a measure that may be absent, and a
/// count that may be absent.
struct Fact {
    region: &'static str,
    dt: &'static str,
    amount: Option<f64>,
    items: Option<i64>,
}

/// The input as the header'd CSV the program reads.
fn facts_csv(facts: &[Fact]) -> String {
    let mut text = String::from("region,dt,amount,items\n");
    for fact in facts {
        text.push_str(&format!(
            "{},{},{},{}\n",
            fact.region,
            fact.dt,
            fact
                .amount
                .map(|value| format!("{value}"))
                .unwrap_or_default(),
            fact.items
                .map(|value| format!("{value}"))
                .unwrap_or_default(),
        ));
    }
    text
}

/// The aggregate the program computes, as this suite computes it independently:
/// `(region, dt) -> (total, items, rows)`, where a group holding no value for a measure
/// reports none of it.
type Groups = BTreeMap<(String, String), (Option<f64>, Option<i64>, i64)>;

fn expected_groups(facts: &[Fact]) -> Groups {
    let mut held: Groups = BTreeMap::new();
    for fact in facts {
        let entry = held
            .entry((fact.region.to_string(), fact.dt.to_string()))
            .or_insert((None, None, 0));
        if let Some(amount) = fact.amount {
            entry.0 = Some(entry.0.unwrap_or(0.0) + amount);
        }
        if let Some(items) = fact.items {
            entry.1 = Some(entry.1.unwrap_or(0) + items);
        }
        entry.2 += 1;
    }
    held
}

/// Every row one Parquet object holds, as `(column name, value as text)` pairs — `None` for a
/// row that holds no value — beside the NULL COUNT each of its column chunks reports in its
/// own statistics. Both are read with the Apache implementation of the format, so what is
/// asserted is that an INDEPENDENT reader parses the bytes this emulator's Spark wrote.
#[allow(clippy::type_complexity)]
fn parquet_rows(
    case: &str,
    key: &str,
    body: &Bytes,
) -> (
    Vec<BTreeMap<String, Option<String>>>,
    BTreeMap<String, u64>,
) {
    use ::parquet::file::reader::{FileReader, SerializedFileReader};
    use ::parquet::record::Field;

    let reader = SerializedFileReader::new(body.clone()).unwrap_or_else(|error| {
        panic!("case {case:?}: {key} must be a Parquet file, got {error} for {body:?}")
    });
    let metadata = reader.metadata();
    let mut nulls: BTreeMap<String, u64> = BTreeMap::new();
    for group in 0..metadata.num_row_groups() {
        let row_group = metadata.row_group(group);
        for index in 0..row_group.num_columns() {
            let column = row_group.column(index);
            let name = column.column_path().string();
            let statistics = column.statistics().unwrap_or_else(|| {
                panic!("case {case:?}: {key} must report statistics for {name}")
            });
            let counted = statistics.null_count_opt().unwrap_or_else(|| {
                panic!("case {case:?}: {key} must report a null count for {name}")
            });
            *nulls.entry(name).or_insert(0) += counted;
        }
    }
    let mut rows = Vec::new();
    for row in reader
        .get_row_iter(None)
        .unwrap_or_else(|error| panic!("case {case:?}: {key} must hold rows, got {error}"))
    {
        let row = row.unwrap_or_else(|error| {
            panic!("case {case:?}: {key} must hold readable rows, got {error}")
        });
        let mut held = BTreeMap::new();
        for (name, field) in row.get_column_iter() {
            let value = match field {
                Field::Null => None,
                Field::Bool(held) => Some(held.to_string()),
                Field::Long(held) => Some(held.to_string()),
                Field::Int(held) => Some(held.to_string()),
                Field::Double(held) => Some(format!("{held}")),
                Field::Float(held) => Some(format!("{held}")),
                Field::Str(held) => Some(held.clone()),
                other => Some(format!("{other:?}")),
            };
            held.insert(name.clone(), value);
        }
        rows.push(held);
    }
    (rows, nulls)
}

/// RULE: a job run's terminal state is what RUNNING the work its driver submitted came to, so
/// a `sparkSubmit` of a Python application that computes an aggregate, writes it as Parquet
/// and reads it back ends in SUCCESS, and the objects it left are the ones it computed — real
/// Parquet files, under the `name=value` directories the write partitioned them by, holding
/// the values the program's own transformations produced.
///
/// The rows differ along what the rule generalises over: how the output is PARTITIONED (not at
/// all, by one column, by two), the VALUES the groups carry (whole numbers, fractions, a
/// measure no row of a group holds), and the FORM of a partition value (plain text, text a
/// path cannot hold literally, text that reads as a number).
#[tokio::test]
async fn a_submitted_spark_program_round_trips_a_partitioned_parquet_dataset() {
    // (case, the partition columns the write names, the facts the program reads)
    let rows: Vec<(&str, &str, Vec<Fact>)> = vec![
        (
            "an unpartitioned output",
            "",
            vec![
                Fact { region: "eu", dt: "2026-01-01", amount: Some(2.5), items: Some(3) },
                Fact { region: "eu", dt: "2026-01-01", amount: Some(1.5), items: Some(4) },
                Fact { region: "us", dt: "2026-01-02", amount: Some(7.0), items: Some(1) },
            ],
        ),
        (
            "one partition column",
            "region",
            vec![
                Fact { region: "eu", dt: "2026-01-01", amount: Some(10.0), items: Some(2) },
                Fact { region: "us", dt: "2026-01-01", amount: Some(20.0), items: Some(5) },
                Fact { region: "us", dt: "2026-01-02", amount: Some(0.25), items: None },
            ],
        ),
        (
            "two partition columns, one of them read as a number",
            "region,dt",
            vec![
                Fact { region: "42", dt: "2026-01-03", amount: Some(1.0), items: Some(1) },
                Fact { region: "42", dt: "2026-01-04", amount: None, items: Some(2) },
                Fact { region: "7", dt: "2026-01-03", amount: Some(3.5), items: Some(3) },
            ],
        ),
        (
            "a partition value a path cannot hold literally",
            "region",
            vec![
                Fact { region: "eu:west", dt: "2026-02-01", amount: Some(4.0), items: Some(2) },
                Fact { region: "eu", dt: "2026-02-01", amount: Some(6.0), items: Some(8) },
            ],
        ),
    ];

    // Every row's work is SUBMITTED first and read back afterwards: the runs are independent,
    // so they are in flight together and the suite waits out one run's window rather than
    // four.
    let router = emulator();
    given_spark_bucket(&router).await;
    put_object(&router, "program/roundtrip.py", Bytes::from(ROUNDTRIP_PROGRAM)).await;
    let application_id = started_application(&router, "tok-spark").await;
    let mut submitted_runs: Vec<String> = Vec::new();
    for (ordinal, (case, keys, facts)) in rows.iter().enumerate() {
        let source = format!("input/{ordinal}/facts.csv");
        put_object(&router, &source, Bytes::from(facts_csv(facts))).await;
        let submitted = start_job_run(
            &router,
            &application_id,
            json!({
                "clientToken": format!("job-spark-{ordinal}"),
                "executionRoleArn": EXECUTION_ROLE,
                "jobDriver": {
                    "sparkSubmit": {
                        "entryPoint": format!("s3://{SPARK_BUCKET}/program/roundtrip.py"),
                        "entryPointArguments": [
                            format!("s3://{SPARK_BUCKET}/{source}"),
                            format!("s3://{SPARK_BUCKET}/output/{ordinal}"),
                            format!("s3://{SPARK_BUCKET}/report/{ordinal}"),
                            keys,
                        ],
                    }
                },
            }),
        )
        .await;
        assert_eq!(
            submitted.status, 200,
            "case {case:?}: StartJobRun expected 200, got {} with {:?}",
            submitted.status, submitted.body
        );
        submitted_runs.push(
            submitted
                .at("/jobRunId")
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }

    for (ordinal, (case, keys, facts)) in rows.iter().enumerate() {
        let output = format!("output/{ordinal}");
        let report = format!("report/{ordinal}");
        let job_run_id = &submitted_runs[ordinal];

        let settled = settled_job_run(&router, &application_id, job_run_id).await;
        assert_eq!(
            settled.at("/jobRun/state"),
            json!("SUCCESS"),
            "case {case:?}: a program that ran to its end must leave the run in SUCCESS, got \
             {:?} with {:?}",
            settled.at("/jobRun/state"),
            settled.at("/jobRun/stateDetails")
        );

        // What the program WROTE: the objects under the output prefix, which are the output's
        // own part files and the marker a completed write leaves beside them.
        let keys_written = keys_under(&router, &format!("{output}/")).await;
        let parts: Vec<String> = keys_written
            .iter()
            .filter(|key| key.ends_with(".snappy.parquet"))
            .cloned()
            .collect();
        assert!(
            !parts.is_empty(),
            "case {case:?}: the write must leave Parquet part files, got {keys_written:?}"
        );
        assert!(
            keys_written
                .iter()
                .any(|key| key.ends_with("/_SUCCESS")),
            "case {case:?}: a completed write leaves its marker, got {keys_written:?}"
        );

        let partition_columns: Vec<&str> = keys.split(',').filter(|name| !name.is_empty()).collect();
        let expected = expected_groups(facts);
        let mut read_back: BTreeMap<(String, String), (String, String, String)> = BTreeMap::new();
        for key in &parts {
            // The directories between the output prefix and the part file are the partition
            // columns of the output: one level per column the write named, in that order.
            let within = key
                .strip_prefix(&format!("{output}/"))
                .unwrap_or(key)
                .to_string();
            let segments: Vec<&str> = within.split('/').collect();
            assert_eq!(
                segments.len(),
                partition_columns.len() + 1,
                "case {case:?}: expected {} partition director(ies) above {key}, got \
                 {segments:?}",
                partition_columns.len()
            );
            let mut from_the_key: BTreeMap<String, String> = BTreeMap::new();
            for (segment, name) in segments.iter().zip(partition_columns.iter()) {
                let (held, value) = segment.split_once('=').unwrap_or_else(|| {
                    panic!("case {case:?}: {segment:?} must name a partition column")
                });
                assert_eq!(
                    held, *name,
                    "case {case:?}: expected the directory of {name}, got {segment:?}"
                );
                assert!(
                    !value.contains(':') && !value.contains('/'),
                    "case {case:?}: a partition value carrying a character the Hive layout \
                     escapes must be written escaped, got {segment:?}"
                );
                from_the_key.insert(held.to_string(), unescape_partition(value));
            }
            let body = get_object(&router, key).await;
            let (rows_held, nulls) = parquet_rows(case, key, &body);
            // The statistics a column chunk reports are computed from the chunk's own values,
            // so the null count of each column is how many of the rows in THIS file hold no
            // value for it.
            for (name, counted) in &nulls {
                let expected_nulls = rows_held
                    .iter()
                    .filter(|row| row.get(name).map(Option::is_none).unwrap_or(false))
                    .count() as u64;
                assert_eq!(
                    *counted, expected_nulls,
                    "case {case:?}: {key} must report {expected_nulls} null(s) for {name}, got \
                     {counted}"
                );
            }
            for row in rows_held {
                let mut values: BTreeMap<String, String> = row
                    .iter()
                    .map(|(name, value)| {
                        (name.clone(), value.clone().unwrap_or_default())
                    })
                    .collect();
                for (name, value) in &from_the_key {
                    values.insert(name.clone(), value.clone());
                }
                for name in &partition_columns {
                    assert!(
                        !row.contains_key(*name),
                        "case {case:?}: a partition column lives in the directory, not in the \
                         file, got {row:?}"
                    );
                }
                read_back.insert(
                    (
                        values.get("region").cloned().unwrap_or_default(),
                        values.get("dt").cloned().unwrap_or_default(),
                    ),
                    (
                        values.get("total").cloned().unwrap_or_default(),
                        values.get("items").cloned().unwrap_or_default(),
                        values.get("rows").cloned().unwrap_or_default(),
                    ),
                );
            }
        }
        assert_eq!(
            read_back.len(),
            expected.len(),
            "case {case:?}: expected {} group(s) in the output, got {read_back:?}",
            expected.len()
        );
        for (group, (total, items, count)) in &expected {
            let found = read_back.get(group).unwrap_or_else(|| {
                panic!("case {case:?}: the output must hold the group {group:?}, got {read_back:?}")
            });
            let expected_total = total
                .map(|value| format!("{value}"))
                .unwrap_or_default();
            let expected_items = items.map(|value| value.to_string()).unwrap_or_default();
            assert_eq!(
                found.0, expected_total,
                "case {case:?}: group {group:?} must total {expected_total:?}, got {found:?}"
            );
            assert_eq!(
                found.1, expected_items,
                "case {case:?}: group {group:?} must count {expected_items:?} item(s), got \
                 {found:?}"
            );
            assert_eq!(
                found.2,
                count.to_string(),
                "case {case:?}: group {group:?} must hold {count} row(s), got {found:?}"
            );
        }

        // ... and what the program REPORTED of its own read back, which is a second output it
        // wrote through the same session.
        let report_keys = keys_under(&router, &format!("{report}/")).await;
        let mut reported = String::new();
        for key in report_keys.iter().filter(|key| !key.ends_with("/_SUCCESS")) {
            reported.push_str(&String::from_utf8_lossy(&get_object(&router, key).await));
        }
        let reported: Value = serde_json::from_str(reported.trim()).unwrap_or_else(|error| {
            panic!("case {case:?}: the program's report must be JSON ({error}), got {reported:?}")
        });
        assert_eq!(
            reported["groups"],
            json!(expected.len()),
            "case {case:?}: the program read back {} group(s), got {reported:?}",
            expected.len()
        );
        let eu_groups = expected
            .keys()
            .filter(|(region, _dt)| region == "eu")
            .count();
        assert_eq!(
            reported["eu"],
            json!(eu_groups),
            "case {case:?}: the program's filter must answer {eu_groups} group(s), got \
             {reported:?}"
        );
        let columns = reported["columns"].as_str().unwrap_or_default();
        for name in ["region", "dt", "total", "items", "rows"] {
            assert!(
                columns.split(',').any(|held| held == name),
                "case {case:?}: the read-back dataset must carry {name}, got {columns:?}"
            );
        }
        // A partition column is read back out of the DIRECTORY that names it, and the type it
        // carries is the one inferred from the value spelled there — a whole number where it
        // spells one, and text otherwise, which is what Spark's partition-column type
        // inference answers.
        let types = reported["types"].as_str().unwrap_or_default();
        for name in &partition_columns {
            let all_numeric = facts
                .iter()
                .map(|fact| if *name == "region" { fact.region } else { fact.dt })
                .all(|value| value.parse::<i64>().is_ok());
            let expected_type = if all_numeric { "bigint" } else { "string" };
            assert!(
                types
                    .split(',')
                    .any(|held| held == format!("{name}:{expected_type}")),
                "case {case:?}: the partition column {name} must be read back as \
                 {expected_type}, got {types:?}"
            );
        }
    }
}

/// The text a percent-encoded partition value spells, which is how the suite reads one back
/// out of a directory name — decoded by the maintained codec this crate already percent-codes
/// with rather than by hand.
fn unescape_partition(value: &str) -> String {
    ::percent_encoding::percent_decode_str(value)
        .decode_utf8_lossy()
        .to_string()
}

/// RULE: a submitted program reading a source whose bytes are not a readable Parquet file
/// FAILS with what the read raised, inside the bounds the codec holds a file's own claims to —
/// so a malformed, truncated or oversized file ends that one run and leaves the emulator
/// answering, rather than being read as rows nothing wrote.
///
/// The rows differ along how the bytes are unreadable: not the format at all, the format's
/// magic with nothing behind it, a real file cut short, a real file whose footer claims a
/// length the file does not hold, and one whose own page claims more values than it carries.
#[tokio::test]
async fn a_submitted_program_reading_unreadable_parquet_fails_within_the_codecs_bounds() {
    let router = emulator();
    given_spark_bucket(&router).await;

    // A real Parquet file, written by this emulator's own Spark, is what the truncated and
    // the tampered rows below are made from.
    let source = "malformed/facts.csv";
    let written = "malformed/written";
    put_object(
        &router,
        source,
        Bytes::from(facts_csv(&[Fact {
            region: "eu",
            dt: "2026-03-01",
            amount: Some(1.5),
            items: Some(2),
        }])),
    )
    .await;
    put_object(&router, "malformed/write.py", Bytes::from(ROUNDTRIP_PROGRAM)).await;
    let application_id = started_application(&router, "tok-malformed").await;
    let writing = start_job_run(
        &router,
        &application_id,
        json!({
            "clientToken": "job-malformed-write",
            "executionRoleArn": EXECUTION_ROLE,
            "jobDriver": {
                "sparkSubmit": {
                    "entryPoint": format!("s3://{SPARK_BUCKET}/malformed/write.py"),
                    "entryPointArguments": [
                        format!("s3://{SPARK_BUCKET}/{source}"),
                        format!("s3://{SPARK_BUCKET}/{written}"),
                        format!("s3://{SPARK_BUCKET}/malformed/report"),
                        "",
                    ],
                }
            },
        }),
    )
    .await;
    let writing_id = writing.at("/jobRunId").as_str().unwrap_or_default().to_string();
    let wrote = settled_job_run(&router, &application_id, &writing_id).await;
    assert_eq!(
        wrote.at("/jobRun/state"),
        json!("SUCCESS"),
        "the control write must succeed before the unreadable rows are made from it, got {:?}",
        wrote.at("/jobRun/stateDetails")
    );
    let part = keys_under(&router, &format!("{written}/"))
        .await
        .into_iter()
        .find(|key| key.ends_with(".snappy.parquet"))
        .expect("the control write leaves a Parquet part file");
    let real = get_object(&router, &part).await.to_vec();

    // (case, the bytes the program reads)
    let mut tampered_footer = real.clone();
    let footer_at = tampered_footer.len() - 8;
    tampered_footer[footer_at..footer_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut tampered_values = real.clone();
    // The page header of the first column chunk opens right after the magic; raising the
    // value count it states makes the chunk claim more values than its page carries.
    for index in 4..tampered_values.len().min(64) {
        if tampered_values[index] == 0x02 {
            tampered_values[index] = 0x7E;
            break;
        }
    }
    let rows: Vec<(&str, Vec<u8>)> = vec![
        ("bytes that are not the format at all", b"not a parquet file".to_vec()),
        ("the magic with nothing behind it", b"PAR1PAR1".to_vec()),
        ("a real file cut short", real[..real.len() / 2].to_vec()),
        ("a footer claiming a length the file does not hold", tampered_footer),
        ("a page claiming more values than it carries", tampered_values),
    ];

    put_object(
        &router,
        "malformed/read.py",
        Bytes::from(
            "import sys\n\nfrom pyspark.sql import SparkSession\n\nspark = \
             SparkSession.builder.appName('read').getOrCreate()\n\
             rows = spark.read.parquet(sys.argv[1]).collect()\n\
             raise ValueError('the read answered %d row(s)' % (len(rows),))\n",
        ),
    )
    .await;

    // Every row's read is SUBMITTED first and read back afterwards, so the runs are in flight
    // together and the suite waits out one run's window rather than five.
    let mut submitted_reads: Vec<String> = Vec::new();
    for (ordinal, (_case, body)) in rows.iter().enumerate() {
        let key = format!("malformed/{ordinal}/part-00000.snappy.parquet");
        put_object(&router, &key, Bytes::from(body.clone())).await;
        let submitted = start_job_run(
            &router,
            &application_id,
            json!({
                "clientToken": format!("job-malformed-{ordinal}"),
                "executionRoleArn": EXECUTION_ROLE,
                "jobDriver": {
                    "sparkSubmit": {
                        "entryPoint": format!("s3://{SPARK_BUCKET}/malformed/read.py"),
                        "entryPointArguments": [
                            format!("s3://{SPARK_BUCKET}/malformed/{ordinal}/")
                        ],
                    }
                },
            }),
        )
        .await;
        submitted_reads.push(
            submitted
                .at("/jobRunId")
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }

    for (ordinal, (case, _body)) in rows.iter().enumerate() {
        let settled = settled_job_run(&router, &application_id, &submitted_reads[ordinal]).await;
        assert_eq!(
            settled.at("/jobRun/state"),
            json!("FAILED"),
            "case {case:?}: a read of unreadable bytes must end the run in the modelled \
             failure state, got {:?} with {:?}",
            settled.at("/jobRun/state"),
            settled.at("/jobRun/stateDetails")
        );
        let details = settled.at("/jobRun/stateDetails");
        let details = details.as_str().unwrap_or_default();
        assert!(
            !details.contains("row(s)"),
            "case {case:?}: no row may be answered for bytes nothing could be read from, got \
             {details:?}"
        );
        // ... and the emulator answers the next call, so nothing of the refusal took the
        // service with it.
        assert_eq!(
            get_application(&router, &application_id).await.status,
            200,
            "case {case:?}: the service must still answer after a refused read"
        );
    }
}

/// The program the join test submits: it matches two datasets it builds itself on the column
/// they share, under each kind of join, and reports what each one answered — so what the suite
/// asserts is what the engine COMPUTED over the program's own rows.
const JOIN_PROGRAM: &str = r#"
import sys

from pyspark.sql import SparkSession

report = sys.argv[1]
spark = SparkSession.builder.appName("joins").getOrCreate()
left = spark.createDataFrame(
    [
        {"region": "eu", "amount": 1},
        {"region": "us", "amount": 2},
        {"region": "us", "amount": 3},
        {"region": "ap", "amount": 4},
    ]
).cache()
right = spark.createDataFrame(
    [
        {"region": "eu", "tax": 10},
        {"region": "us", "tax": 20},
        {"region": "za", "tax": 30},
    ]
)

answered = {}
for how in (
    "inner",
    "cross",
    "left",
    "left_outer",
    "right",
    "right_outer",
    "full",
    "full_outer",
    "outer",
    "leftsemi",
    "left_semi",
    "semi",
    "leftanti",
    "left_anti",
    "anti",
):
    joined = left.join(right, "region", how)
    answered[how] = joined.count()
    answered["%s_columns" % (how,)] = ",".join(joined.columns)

# A `how` naming no kind of join is REFUSED rather than answered as some other join.
try:
    left.join(right, "region", "innr")
    answered["refused"] = ""
except Exception as error:
    answered["refused"] = "%s: %s" % (type(error).__name__, error)

matched = left.join(right, ["region"], "inner").select("*").orderBy("region", "amount")
answered["matched"] = ";".join(
    "%s:%s:%s" % (row["region"], row["amount"], row["tax"]) for row in matched.collect()
)
answered["regions"] = left.select("region").dropDuplicates().count()
answered["unmatched_is_empty"] = int(left.join(right, "region", "leftanti").isEmpty())
answered["first"] = matched.head()["region"]

spark.createDataFrame([answered]).coalesce(1).write.mode("overwrite").json(report)
"#;

/// RULE: what a submitted program's own transformations answer is what this engine COMPUTES
/// over its rows — so a program that matches two datasets on the column they share gets, for
/// each kind of join, the rows that kind keeps and the columns it carries, and the dataset it
/// then holds, de-duplicates and orders is the one its own values make.
///
/// The cases differ along what the rule generalises over: the KIND of join (inner, the three
/// outer ones, and the two semi-joins), whether a row of either side matches, and whether a
/// key matches more than one row on the other side.
#[tokio::test]
async fn a_submitted_spark_program_matches_two_datasets_on_the_column_they_share() {
    let router = emulator();
    given_spark_bucket(&router).await;
    put_object(&router, "join/program.py", Bytes::from(JOIN_PROGRAM)).await;
    let application_id = started_application(&router, "tok-join").await;
    let submitted = start_job_run(
        &router,
        &application_id,
        json!({
            "clientToken": "job-join",
            "executionRoleArn": EXECUTION_ROLE,
            "jobDriver": {
                "sparkSubmit": {
                    "entryPoint": format!("s3://{SPARK_BUCKET}/join/program.py"),
                    "entryPointArguments": [format!("s3://{SPARK_BUCKET}/join/report")],
                }
            },
        }),
    )
    .await;
    let job_run_id = submitted
        .at("/jobRunId")
        .as_str()
        .unwrap_or_default()
        .to_string();
    let settled = settled_job_run(&router, &application_id, &job_run_id).await;
    assert_eq!(
        settled.at("/jobRun/state"),
        json!("SUCCESS"),
        "a program that ran to its end must leave the run in SUCCESS, got {:?} with {:?}",
        settled.at("/jobRun/state"),
        settled.at("/jobRun/stateDetails")
    );

    let mut reported = String::new();
    for key in keys_under(&router, "join/report/")
        .await
        .iter()
        .filter(|key| !key.ends_with("/_SUCCESS"))
    {
        reported.push_str(&String::from_utf8_lossy(&get_object(&router, key).await));
    }
    let answered: Value = serde_json::from_str(reported.trim()).unwrap_or_else(|error| {
        panic!("the program's report must be JSON ({error}), got {reported:?}")
    });

    // (case, how many rows that kind of join keeps, the columns it carries)
    //
    // The left dataset holds eu/1, us/2, us/3 and ap/4; the right holds eu/10, us/20 and
    // za/30. So an inner join keeps the three matching pairs (the `us` key matching twice),
    // each outer join adds the unmatched rows of the side it keeps, and the two semi-joins
    // answer the rows of the left side that did and did not match. Every spelling
    // `DataFrame.join`'s `how` documents for one kind answers that kind.
    for (case, rows, columns) in [
        ("inner", 3, "region,amount,tax"),
        ("cross", 3, "region,amount,tax"),
        ("left", 4, "region,amount,tax"),
        ("left_outer", 4, "region,amount,tax"),
        ("right", 4, "region,amount,tax"),
        ("right_outer", 4, "region,amount,tax"),
        ("full", 5, "region,amount,tax"),
        ("full_outer", 5, "region,amount,tax"),
        ("outer", 5, "region,amount,tax"),
        ("leftsemi", 3, "region,amount"),
        ("left_semi", 3, "region,amount"),
        ("semi", 3, "region,amount"),
        ("leftanti", 1, "region,amount"),
        ("left_anti", 1, "region,amount"),
        ("anti", 1, "region,amount"),
    ] {
        assert_eq!(
            answered[case],
            json!(rows),
            "case {case:?}: expected {rows} row(s), got {answered:?}"
        );
        assert_eq!(
            answered[format!("{case}_columns")],
            json!(columns),
            "case {case:?}: expected the columns {columns:?}, got {answered:?}"
        );
    }
    assert_eq!(
        answered["matched"],
        json!("eu:1:10;us:2:20;us:3:20"),
        "the matched rows must carry both sides' values, in the order asked for, got \
         {answered:?}"
    );
    assert_eq!(
        answered["regions"],
        json!(3),
        "the left dataset holds three distinct regions, got {answered:?}"
    );
    assert_eq!(
        answered["unmatched_is_empty"],
        json!(0),
        "one row of the left dataset matched nothing, so the anti-join is not empty, got \
         {answered:?}"
    );
    assert_eq!(
        answered["first"],
        json!("eu"),
        "the first row of the ordered match is the least region, got {answered:?}"
    );
    // ... and a kind of join this engine does not match is refused, naming the kinds it does,
    // rather than answering the rows of some other join.
    let refused = answered["refused"].as_str().unwrap_or_default();
    assert!(
        refused.starts_with("AnalysisException: Unsupported join type 'innr'.")
            && refused.contains("'LeftSemi'"),
        "a join kind this engine does not match must be refused naming the kinds it does, \
         got {refused:?}"
    );
}

/// RULE: a submitted job run passes through the non-terminal `JobRunState` values — `RUNNING`
/// among them — and stays in them for longer than the interval a client that submits work and
/// then looks for it polls at, so such a client observes the run IN FLIGHT: `GetJobRun`
/// reports a non-terminal state and a `ListJobRuns` filtered on the non-terminal states
/// reports the run, whatever the run's own workload turns out to be.
///
/// The rows differ along what the rule generalises over: what the submitted work IS — an entry
/// point the object store holds nothing at, a program that computes and ends at once, and one
/// that asks to wait past the span one run may last — because the window a run is observable
/// in flight for is the clock's and not the workload's.
#[tokio::test]
async fn a_submitted_job_run_is_observable_in_flight_past_a_client_poll_interval() {
    assert!(
        non_terminal_job_run_window() > CLIENT_POLL_INTERVAL,
        "a run must stay non-terminal for longer than the {CLIENT_POLL_INTERVAL:?} a client \
         looks at, got {:?}",
        non_terminal_job_run_window()
    );
    assert!(
        JOB_RUN_STAGES
            .iter()
            .any(|(state, _details)| *state == "RUNNING"),
        "RUNNING must be among the states a run passes through, got {:?}",
        JOB_RUN_STAGES
            .iter()
            .map(|(state, _)| *state)
            .collect::<Vec<&str>>()
    );

    // (case, the program the submit names, and whether the object store holds it)
    let rows: [(&str, &str, bool); 3] = [
        ("an entry point the store holds nothing at", "", false),
        (
            "a program that computes and ends at once",
            "value = sum(index for index in range(10))\n",
            true,
        ),
        (
            "a program that waits past the span one run may last",
            "import time\ntime.sleep(240)\n",
            true,
        ),
    ];

    // Every row's work is SUBMITTED first and the runs are then polled TOGETHER, so the suite
    // waits out one run's window rather than one per row, and each run is still observed
    // through every state it passes.
    let router = emulator();
    given_spark_bucket(&router).await;
    let application_id = started_application(&router, "tok-flight").await;
    let mut runs: Vec<(usize, String, Vec<String>)> = Vec::new();
    for (ordinal, (_case, program, staged)) in rows.iter().enumerate() {
        let key = format!("in-flight/{ordinal}/app.py");
        if *staged {
            put_object(&router, &key, Bytes::from(program.to_string())).await;
        }
        let submitted = start_job_run(
            &router,
            &application_id,
            json!({
                "clientToken": format!("job-flight-{ordinal}"),
                "executionRoleArn": EXECUTION_ROLE,
                "jobDriver": {
                    "sparkSubmit": { "entryPoint": format!("s3://{SPARK_BUCKET}/{key}") }
                },
            }),
        )
        .await;
        runs.push((
            ordinal,
            submitted
                .at("/jobRunId")
                .as_str()
                .unwrap_or_default()
                .to_string(),
            Vec::new(),
        ));
    }

    // Every state each run is observed in, from the submit until it comes to rest: the look is
    // a POLL for the next change rather than a wait of any length.
    let deadline = Instant::now() + JOB_RUN_REST_DEADLINE;
    loop {
        let mut resting = 0;
        for (ordinal, job_run_id, observed) in runs.iter_mut() {
            let case = rows[*ordinal].0;
            let answer = get_job_run(&router, &application_id, job_run_id).await;
            let state = answer
                .at("/jobRun/state")
                .as_str()
                .unwrap_or_default()
                .to_string();
            if observed.last() != Some(&state) {
                // The listing a client filters on the non-terminal states must report the run
                // for as long as it is in one of them, which is what makes it observable at
                // all.
                if !TERMINAL_JOB_RUN_STATES.contains(&state.as_str()) {
                    let filter = IN_FLIGHT_STATES
                        .iter()
                        .map(|held| format!("states={held}"))
                        .collect::<Vec<String>>()
                        .join("&");
                    let listed =
                        list_job_runs(&router, &application_id, &format!("?{filter}")).await;
                    let reported: Vec<String> = listed
                        .body
                        .get("jobRuns")
                        .and_then(Value::as_array)
                        .unwrap_or(&Vec::new())
                        .iter()
                        .map(|entry| entry["id"].as_str().unwrap_or_default().to_string())
                        .collect();
                    assert!(
                        reported.contains(job_run_id),
                        "case {case:?}: a run in {state} must be reported by a listing \
                         filtered on the non-terminal states, got {reported:?}"
                    );
                }
                observed.push(state.clone());
            }
            if TERMINAL_JOB_RUN_STATES.contains(&state.as_str()) {
                resting += 1;
            }
        }
        if resting == runs.len() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a submitted run never came to rest, having been {:?}",
            runs.iter()
                .map(|(_ordinal, _id, observed)| observed.clone())
                .collect::<Vec<Vec<String>>>()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    for (ordinal, _job_run_id, observed) in &runs {
        let case = rows[*ordinal].0;
        for (state, _details) in JOB_RUN_STAGES {
            assert!(
                observed.iter().any(|held| held == state),
                "case {case:?}: a client polling the run must observe it {state}, got \
                 {observed:?}"
            );
        }
        assert_eq!(
            observed.first().map(String::as_str),
            Some("SUBMITTED"),
            "case {case:?}: a run is SUBMITTED when it is accepted, got {observed:?}"
        );
    }
}
