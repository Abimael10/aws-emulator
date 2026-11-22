// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `ListJobExecutionsForJob`: the control plane lists the job
//! executions ONE JOB has, on whichever device each was queued on, computed from the SHARED
//! job-execution store (`service_common::iot_job_executions`) the IoT Jobs data plane
//! serves.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `GET /jobs/{jobId}/things` signed for the `iot` signing name — after creating the jobs
//! through `PUT /jobs/{jobId}`, so what is listed is what a writer persisted.
//!
//! The rules under test:
//!
//! * the listing reports one entry per execution the store holds for THAT job, each the
//!   stored record projected onto `JobExecutionSummaryForJob` — `thingArn` plus
//!   `jobExecutionSummary`'s `status`, `queuedAt`, `startedAt`, `lastUpdatedAt`,
//!   `executionNumber` and `retryAttempt` — in the store's enqueue order, and reading it
//!   changes no record;
//! * one job's listing reports that job's executions and no other job's, whichever devices
//!   the two jobs share;
//! * `status` narrows the listing to the executions it selects, and a filter nothing
//!   matches is an EMPTY list, not a refusal;
//! * `maxResults` and `nextToken` walk that one order, every execution exactly once, and a
//!   `nextToken` that is not a token of THIS listing is refused 400
//!   `InvalidRequestException`;
//! * a `jobId` naming no job of the account is refused 404 `ResourceNotFoundException` with
//!   the jobs plane's own sentence;
//! * an execution whose armed timer the clock has already passed is listed `TIMED_OUT` —
//!   the transition that was due at the deadline — while a twin whose timer is still in the
//!   future is untouched, and a COMMAND execution the same store holds is not listed as an
//!   execution of the job.
//!
//! Each table spans the class a rule generalises over — how many devices a job targets, how
//! the targets are ordered against their names, every `JobExecutionStatus` an execution can
//! be read in, each published `maxResults` bound and one past it, and the forms an id that
//! names no job takes — rather than one recorded request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use service_common::iot_job_executions::{
    now_epoch_seconds, JobExecutionRecord, JobExecutionStore, STATUS_IN_PROGRESS, STATUS_QUEUED,
    STATUS_REJECTED, STATUS_TIMED_OUT,
};
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// The values `JobExecutionStatus` admits
/// (`smithy_query(query="shape",shape="JobExecutionStatus")`), which is the catalog the
/// `status` filter is driven over.
const JOB_EXECUTION_STATUSES: &[&str] = &[
    "QUEUED",
    "IN_PROGRESS",
    "SUCCEEDED",
    "FAILED",
    "TIMED_OUT",
    "REJECTED",
    "REMOVED",
    "CANCELED",
];

/// The largest page `LaserMaxResults` admits (`range 1..250`).
const MAX_RESULTS_CEILING: i32 = 250;

/// The longest `JobId` the model admits (`len 1..64`), used as one id that names no job.
const MAX_JOB_ID_LEN: usize = 64;

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name: the
/// scope is how the router picks the claiming service.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The listed entries, or an empty slice when the member is absent.
    fn entries(&self) -> &[Value] {
        self.body["executionSummaries"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
    }

    /// The listed `thingArn`s, in the order they were reported.
    fn thing_arns(&self) -> Vec<String> {
        self.entries()
            .iter()
            .map(|entry| entry["thingArn"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// One listed entry's `jobExecutionSummary` member, or `Value::Null`.
    fn summary_member(&self, index: usize, name: &str) -> Value {
        self.entries()
            .get(index)
            .map(|entry| entry["jobExecutionSummary"][name].clone())
            .unwrap_or(Value::Null)
    }

    fn next_token(&self) -> Option<String> {
        self.body
            .get("nextToken")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it. The
/// engine handle is kept so a case can seed and read the SHARED job-execution store the
/// control plane writes into and the data plane serves.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    async fn create_thing(&self, thing_name: &str) -> Reply {
        self.call("POST", &format!("/things/{thing_name}"), Some(&json!({})))
            .await
    }

    /// `CreateJob` targeting every device in `things`, in that order, which is what creates
    /// one `QUEUED` execution per device.
    async fn create_job(&self, job_id: &str, things: &[&str]) -> Reply {
        let request = json!({
            "targets": things.iter().map(|thing| thing_arn(thing)).collect::<Vec<String>>(),
            "document": format!(r#"{{"operation":"reboot","job":"{job_id}"}}"#),
        });
        let created = self
            .call("PUT", &format!("/jobs/{job_id}"), Some(&request))
            .await;
        assert_eq!(
            created.status, 200,
            "creating job {job_id:?} must answer 200, got {} {:?}",
            created.status, created.body
        );
        created
    }

    /// `ListJobExecutionsForJob` for one job, with `query` appended to the URI (where
    /// `status`, `maxResults` and `nextToken` are bound).
    async fn list_with_query(&self, job_id: &str, query: &str) -> Reply {
        self.call("GET", &format!("/jobs/{job_id}/things{query}"), None)
            .await
    }

    async fn list(&self, job_id: &str) -> Reply {
        self.list_with_query(job_id, "").await
    }

    /// `DescribeJobExecution` for a (thing, job) pair: the single-execution reader whose
    /// answer a listed entry must agree with.
    async fn describe(&self, thing_name: &str, job_id: &str) -> Reply {
        self.call("GET", &format!("/things/{thing_name}/jobs/{job_id}"), None)
            .await
    }

    /// Mutate the stored execution of (thing, job) in the SHARED store, so a case can put a
    /// record in a state only the device-facing plane reaches (a status, a start instant, an
    /// armed timer) and then list it back through this service.
    fn with_stored_execution<F>(&self, thing_name: &str, job_id: &str, mutate: F)
    where
        F: FnOnce(&mut JobExecutionRecord),
    {
        let store = self.engine.state::<JobExecutionStore>();
        let mut store = store.lock();
        let index = store
            .execution_index(thing_name, job_id, None)
            .unwrap_or_else(|| panic!("{thing_name} must hold an execution of {job_id}"));
        mutate(
            store
                .record_mut(thing_name, index)
                .expect("the index came from this store"),
        );
    }

    /// Put one COMMAND execution in the SHARED store, exactly as the data plane's
    /// `StartCommandExecution` does: a `QUEUED` record keyed by the `executionId` it minted,
    /// carrying the `commandArn` that makes it a command execution.
    fn seed_command_execution(&self, thing_name: &str, execution_id: &str, command_id: &str) {
        let mut record = JobExecutionRecord::queued(
            execution_id.to_string(),
            thing_name.to_string(),
            None,
            now_epoch_seconds(),
        );
        record.command_arn = Some(format!(
            "arn:aws:iot:{REGION}:{ACCOUNT_ID}:command/{command_id}"
        ));
        let store = self.engine.state::<JobExecutionStore>();
        let mut store = store.lock();
        store
            .try_insert_execution(record)
            .unwrap_or_else(|refusal| panic!("seeding {execution_id}: {refusal}"));
    }

    /// The status the SHARED store holds for (thing, job) — what the device polling the Jobs
    /// data plane would see.
    fn stored_status(&self, thing_name: &str, job_id: &str) -> String {
        let store = self.engine.state::<JobExecutionStore>();
        let store = store.lock();
        store
            .execution_index(thing_name, job_id, None)
            .and_then(|index| store.record(thing_name, index))
            .map(|record| record.status.clone())
            .unwrap_or_default()
    }
}

/// The ARN of a thing: what a `targets` entry names a device by, and what a listed entry
/// reports it as.
fn thing_arn(thing_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing_name}")
}

/// The ARNs of `things`, in that order — one expected listing.
fn thing_arns(things: &[&str]) -> Vec<String> {
    things.iter().map(|thing| thing_arn(thing)).collect()
}

/// One row of the listing table: a job and the devices it targets, in target order.
struct JobCase {
    case: &'static str,
    job_id: &'static str,
    things: &'static [&'static str],
}

/// RULE: the listing reports every execution the store holds for the named job, each the
/// stored record projected onto `JobExecutionSummaryForJob`, in the store's enqueue order —
/// and one job's listing reports no other job's executions.
#[tokio::test]
async fn a_jobs_executions_are_listed_from_the_store() {
    let emulator = Emulator::new();

    // The rows differ in how many devices one job targets, in whether the target order
    // agrees with the devices' names (so the reported order is the enqueue order and not an
    // alphabetical one), and in whether two jobs share a device.
    let jobs: &[JobCase] = &[
        JobCase {
            case: "one device",
            job_id: "forjob-one",
            things: &["forjob-m"],
        },
        JobCase {
            case: "three devices, named against the target order",
            job_id: "forjob-three",
            things: &["forjob-z", "forjob-a", "forjob-m"],
        },
        JobCase {
            case: "a device the previous jobs already targeted",
            job_id: "forjob-shared",
            things: &["forjob-m", "forjob-z"],
        },
    ];

    for thing in ["forjob-a", "forjob-m", "forjob-z"] {
        emulator.create_thing(thing).await;
    }
    let before = now_epoch_seconds();
    for job in jobs {
        let created = emulator.create_job(job.job_id, job.things).await;
        assert_eq!(
            created.body["jobId"].as_str(),
            Some(job.job_id),
            "case {:?}: the create must report the job it made, got {:?}",
            job.case,
            created.body
        );
    }
    let after = now_epoch_seconds();

    for job in jobs {
        let listed = emulator.list(job.job_id).await;
        assert_eq!(
            listed.status, 200,
            "case {:?}: listing {}'s executions must answer 200, got {} {:?}",
            job.case, job.job_id, listed.status, listed.body
        );
        assert_eq!(
            listed.thing_arns(),
            thing_arns(job.things),
            "case {:?}: expected the executions of {} on {:?} in enqueue order, got {:?}",
            job.case,
            job.job_id,
            job.things,
            listed.body
        );
        assert_eq!(
            listed.next_token(),
            None,
            "case {:?}: a page holding every execution carries no nextToken, got {:?}",
            job.case,
            listed.body
        );

        for index in 0..job.things.len() {
            let queued_at = listed.summary_member(index, "queuedAt").as_i64();
            assert!(
                queued_at.is_some_and(|seconds| (before..=after).contains(&seconds)),
                "case {:?}: entry {index}'s queuedAt must be when the job was created \
                 ({before}..={after}), got {:?}",
                job.case,
                listed.summary_member(index, "queuedAt")
            );
            assert_eq!(
                (
                    listed.summary_member(index, "status"),
                    listed.summary_member(index, "executionNumber"),
                    listed.summary_member(index, "lastUpdatedAt"),
                    listed.summary_member(index, "startedAt"),
                    listed.summary_member(index, "retryAttempt"),
                ),
                (
                    json!(STATUS_QUEUED),
                    json!(1),
                    json!(queued_at),
                    Value::Null,
                    Value::Null,
                ),
                "case {:?}: entry {index} must be the queued execution the create made, \
                 with no start and no retry, got {:?}",
                job.case,
                listed.entries().get(index)
            );
        }

        // ROUND TRIP: the single-execution reader of each listed record agrees with the
        // listing, down to the ARN of the device it runs on.
        for (index, thing) in job.things.iter().enumerate() {
            let read = emulator.describe(thing, job.job_id).await;
            assert_eq!(
                (
                    read.status,
                    read.body["execution"]["thingArn"].as_str(),
                    read.body["execution"]["status"].as_str(),
                    read.body["execution"]["queuedAt"].as_i64(),
                ),
                (
                    200,
                    Some(thing_arn(thing).as_str()),
                    Some(STATUS_QUEUED),
                    listed.summary_member(index, "queuedAt").as_i64(),
                ),
                "case {:?}: DescribeJobExecution must report the listed execution of \
                 {thing}, got {} {:?}",
                job.case,
                read.status,
                read.body
            );
        }
    }
}

/// RULE: `status` narrows the listing to the executions of this job in that status; a filter
/// nothing matches is an empty list.
#[tokio::test]
async fn the_status_filter_selects_executions() {
    let emulator = Emulator::new();
    let job_id = "status-filter-job";

    // One device per `JobExecutionStatus` value, each of this job's executions moved to that
    // status in the SHARED store the way the device-facing plane moves it — so the table
    // covers the whole catalog, not only the status a create leaves behind.
    let devices: Vec<String> = JOB_EXECUTION_STATUSES
        .iter()
        .map(|status| format!("status-{}", status.to_lowercase().replace('_', "-")))
        .collect();
    for device in &devices {
        emulator.create_thing(device).await;
    }
    let targets: Vec<&str> = devices.iter().map(String::as_str).collect();
    emulator.create_job(job_id, &targets).await;
    for (device, status) in devices.iter().zip(JOB_EXECUTION_STATUSES) {
        emulator.with_stored_execution(device, job_id, |record| {
            record.status = (*status).to_string();
        });
    }
    // A second job on one of the same devices, left QUEUED: it must never be selected by
    // this job's listing, whatever the filter.
    emulator.create_job("status-other-job", &[devices[0].as_str()]).await;

    for (device, status) in devices.iter().zip(JOB_EXECUTION_STATUSES) {
        let listed = emulator
            .list_with_query(job_id, &format!("?status={status}"))
            .await;
        assert_eq!(
            (listed.status, listed.thing_arns()),
            (200, vec![thing_arn(device)]),
            "case {status:?}: the status filter must select exactly that execution, got {} {:?}",
            listed.status,
            listed.body
        );
        assert_eq!(
            listed.summary_member(0, "status"),
            json!(status),
            "case {status:?}: the selected entry must report that status, got {:?}",
            listed.entries().first()
        );
    }

    // (case, query, the devices whose executions it must report)
    let rows: &[(&str, String, Vec<String>)] = &[
        (
            "no filter at all",
            String::new(),
            devices.iter().map(|device| thing_arn(device)).collect(),
        ),
        (
            "one status with the largest page size",
            format!("?status={STATUS_IN_PROGRESS}&maxResults={MAX_RESULTS_CEILING}"),
            vec![thing_arn(&devices[1])],
        ),
    ];
    for (case, query, expected) in rows {
        let listed = emulator.list_with_query(job_id, query).await;
        assert_eq!(
            (listed.status, listed.thing_arns()),
            (200, expected.clone()),
            "case {case:?}: expected {expected:?}, got {} {:?}",
            listed.status,
            listed.body
        );
    }

    // A filter the other job's single QUEUED execution does not match: an EMPTY list, not a
    // refusal, and not the executions another job holds in that status.
    let empty = emulator
        .list_with_query("status-other-job", "?status=SUCCEEDED")
        .await;
    assert_eq!(
        (empty.status, empty.thing_arns(), empty.next_token()),
        (200, Vec::new(), None),
        "case \"a filter nothing matches\": expected an empty page, got {} {:?}",
        empty.status,
        empty.body
    );
    let other = emulator.list("status-other-job").await;
    assert_eq!(
        (other.status, other.thing_arns()),
        (200, vec![thing_arn(&devices[0])]),
        "case \"the other job\": expected only its own execution, got {} {:?}",
        other.status,
        other.body
    );
}

/// RULE: `maxResults` and `nextToken` walk ONE order — every execution of the job exactly
/// once, whatever the page size — and a `nextToken` that is not a token of this listing is
/// refused `InvalidRequestException` (400).
#[tokio::test]
async fn pages_walk_every_execution_exactly_once() {
    let emulator = Emulator::new();
    let job_id = "paging-job";

    let devices: Vec<String> = (0..7).map(|index| format!("paging-device-{index}")).collect();
    for device in &devices {
        emulator.create_thing(device).await;
    }
    let targets: Vec<&str> = devices.iter().map(String::as_str).collect();
    emulator.create_job(job_id, &targets).await;
    let expected: Vec<String> = devices.iter().map(|device| thing_arn(device)).collect();

    // Page sizes spanning one execution per page, a size that does not divide the total, the
    // exact total, and each published bound of `LaserMaxResults` (`range 1..250`).
    for page_size in [1, 2, 3, 7, MAX_RESULTS_CEILING] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let query = match token.as_deref() {
                Some(token) => format!("?maxResults={page_size}&nextToken={token}"),
                None => format!("?maxResults={page_size}"),
            };
            let listed = emulator.list_with_query(job_id, &query).await;
            assert_eq!(
                listed.status, 200,
                "case maxResults={page_size}: page {pages} must answer 200, got {} {:?}",
                listed.status, listed.body
            );
            assert!(
                listed.entries().len() <= page_size as usize,
                "case maxResults={page_size}: page {pages} must hold at most {page_size} \
                 entries, got {}",
                listed.entries().len()
            );
            walked.extend(listed.thing_arns());
            pages += 1;
            assert!(
                pages <= devices.len() + 1,
                "case maxResults={page_size}: the walk must terminate, got {pages} pages for \
                 {} executions",
                devices.len()
            );
            match listed.next_token() {
                Some(next) => token = Some(next),
                None => break,
            }
        }
        assert_eq!(
            walked, expected,
            "case maxResults={page_size}: the pages must cover every execution exactly once \
             in one order, got {walked:?}"
        );
    }

    // A token this listing issued, replayed against a DIFFERENT filter set: it names a
    // position of another listing, so it is refused rather than resuming that one.
    let first_page = emulator.list_with_query(job_id, "?maxResults=2").await;
    let token = first_page
        .next_token()
        .expect("a truncated listing carries a nextToken");

    // (case, query)
    let refused: &[(&str, String)] = &[
        (
            "a nextToken that is not a token of any listing",
            "?nextToken=not-a-token".to_string(),
        ),
        (
            "a nextToken that is hex but names no position",
            format!("?nextToken={}", ::hex::encode("nonsense")),
        ),
        (
            "this listing's token under a status filter",
            format!("?maxResults=2&status={STATUS_QUEUED}&nextToken={token}"),
        ),
    ];
    for (case, query) in refused {
        let listed = emulator.list_with_query(job_id, query).await;
        assert_eq!(
            (listed.status, listed.error_code.as_deref()),
            (
                400,
                Some("InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/"),
            ),
            "case {case:?}: expected 400 InvalidRequestException, got {} {:?} with {:?}",
            listed.status,
            listed.error_code,
            listed.body
        );
    }

    // The same token under the listing that issued it still resumes that listing.
    let resumed = emulator
        .list_with_query(job_id, &format!("?maxResults=2&nextToken={token}"))
        .await;
    assert_eq!(
        (resumed.status, resumed.thing_arns()),
        (200, expected[2..4].to_vec()),
        "case \"the token of this listing\": expected the next page, got {} {:?}",
        resumed.status,
        resumed.body
    );

    // Each `LaserMaxResults` bound, and one past each: the range is modelled, so a page size
    // outside it is refused before this listing runs.
    for (case, page_size, expected_status) in [
        ("the lower bound", 1, 200),
        ("one below the lower bound", 0, 400),
        ("the upper bound", MAX_RESULTS_CEILING, 200),
        ("one past the upper bound", MAX_RESULTS_CEILING + 1, 400),
    ] {
        let listed = emulator
            .list_with_query(job_id, &format!("?maxResults={page_size}"))
            .await;
        assert_eq!(
            listed.status, expected_status,
            "case {case:?}: maxResults={page_size} must answer {expected_status}, got {} {:?}",
            listed.status, listed.body
        );
    }
}

/// RULE: a `jobId` naming no job of the account addresses no job at all, so the listing is
/// refused 404 `ResourceNotFoundException` with the jobs plane's own sentence.
#[tokio::test]
async fn a_job_that_does_not_exist_is_refused() {
    let emulator = Emulator::new();
    emulator.create_thing("missing-job-device").await;
    emulator
        .create_job("present-job", &["missing-job-device"])
        .await;

    // The rows differ in how the unknown id relates to the one that does exist: unrelated,
    // a suffixed form of it, its case-shifted form, and the longest id the model admits.
    for (case, job_id) in [
        ("an id nobody created", "absent-job".to_string()),
        ("the known id with a suffix", "present-job-2".to_string()),
        ("the known id in another case", "PRESENT-JOB".to_string()),
        ("the longest id the model admits", "j".repeat(MAX_JOB_ID_LEN)),
    ] {
        let listed = emulator.list(&job_id).await;
        assert_eq!(
            (listed.status, listed.error_code.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {case:?}: expected 404 ResourceNotFoundException, got {} {:?} with {:?}",
            listed.status,
            listed.error_code,
            listed.body
        );
        assert_eq!(
            listed.message(),
            format!("Job {job_id} cannot be found."),
            "case {case:?}: expected the jobs plane's missing-job sentence, got {:?}",
            listed.body
        );
    }

    // The job that DOES exist is listed, so the refusal is about the id and not about the
    // listing itself.
    let listed = emulator.list("present-job").await;
    assert_eq!(
        (listed.status, listed.thing_arns()),
        (200, vec![thing_arn("missing-job-device")]),
        "case \"the job that exists\": expected its one execution, got {} {:?}",
        listed.status,
        listed.body
    );
}

/// RULE: the listing reports the status the CLOCK decided — an execution whose armed timer
/// has passed is `TIMED_OUT`, stamped at the deadline, while a twin whose timer is still
/// ahead is untouched — and a COMMAND execution the same store holds is not an execution of
/// the job.
#[tokio::test]
async fn a_due_timeout_is_reported_and_a_command_execution_is_not_listed() {
    let emulator = Emulator::new();
    let job_id = "timer-job";

    let now = now_epoch_seconds();
    // (case, the device, the deadline its timer is armed at, the status it must be listed in)
    let rows: &[(&str, &str, i64, &str)] = &[
        (
            "a deadline the clock passed",
            "timer-past-device",
            now - 60,
            STATUS_TIMED_OUT,
        ),
        (
            "a deadline reached exactly now",
            "timer-now-device",
            now,
            STATUS_TIMED_OUT,
        ),
        (
            "a deadline still ahead",
            "timer-future-device",
            now + 3_600,
            STATUS_QUEUED,
        ),
    ];

    let devices: Vec<&str> = rows
        .iter()
        .map(|(_, device, _, _)| *device)
        .chain(["timer-rejected-device"])
        .collect();
    for device in &devices {
        emulator.create_thing(device).await;
    }
    emulator.create_job(job_id, &devices).await;
    for (_, device, deadline, _) in rows {
        emulator.with_stored_execution(device, job_id, |record| {
            record.execution_timeout_expires_at = Some(*deadline);
        });
    }
    // A finished execution has no deadline left to reach, so its armed timer moves it
    // nowhere.
    emulator.with_stored_execution("timer-rejected-device", job_id, |record| {
        record.status = STATUS_REJECTED.to_string();
        record.execution_timeout_expires_at = Some(now - 60);
    });
    // Command executions the same store holds: one on a device this job targeted, keyed by
    // an id of its own, and one on another device whose minted id is spelled exactly like
    // this job's. Neither is an execution of the job.
    emulator.seed_command_execution("timer-past-device", "command-execution-1", "reboot-command");
    emulator.seed_command_execution("command-only-device", job_id, "reboot-command");

    let listed = emulator.list(job_id).await;
    assert_eq!(
        listed.status, 200,
        "listing the job's executions must answer 200, got {} {:?}",
        listed.status, listed.body
    );
    assert_eq!(
        listed.thing_arns(),
        thing_arns(&devices),
        "the command executions must not be listed as executions of the job, got {:?}",
        listed.body
    );

    for (index, (case, device, deadline, expected_status)) in rows.iter().enumerate() {
        assert_eq!(
            listed.summary_member(index, "status"),
            json!(expected_status),
            "case {case:?}: expected {device}'s execution listed {expected_status}, got {:?}",
            listed.entries().get(index)
        );
        assert_eq!(
            emulator.stored_status(device, job_id),
            (*expected_status).to_string(),
            "case {case:?}: the device polling the shared store must see the same status"
        );
        if *expected_status == STATUS_TIMED_OUT {
            assert_eq!(
                listed.summary_member(index, "lastUpdatedAt"),
                json!(*deadline),
                "case {case:?}: a timed-out execution is stamped at its deadline {deadline}, \
                 got {:?}",
                listed.entries().get(index)
            );
        }
    }
    assert_eq!(
        (
            listed.summary_member(3, "status"),
            emulator.stored_status("timer-rejected-device", job_id),
        ),
        (json!(STATUS_REJECTED), STATUS_REJECTED.to_string()),
        "case \"a finished execution with an armed timer\": a terminal status is never moved, \
         got {:?}",
        listed.entries().get(3)
    );

    // Reading does not change what a later caller sees: the second listing is the first.
    let again = emulator.list(job_id).await;
    assert_eq!(
        again.body, listed.body,
        "a second listing must report exactly what the first did, got {:?}",
        again.body
    );
    // The command executions are still in the shared store; they are simply not executions
    // of this job.
    assert_eq!(
        (
            emulator.stored_status("timer-past-device", "command-execution-1"),
            emulator.stored_status("command-only-device", job_id),
        ),
        (STATUS_QUEUED.to_string(), STATUS_QUEUED.to_string()),
        "the listing must leave the command executions the data plane serves untouched"
    );

    // A device that starts its work is listed IN_PROGRESS, with the instant it started.
    emulator.with_stored_execution("timer-future-device", job_id, |record| {
        record.start(now);
    });
    let started = emulator
        .list_with_query(job_id, &format!("?status={STATUS_IN_PROGRESS}"))
        .await;
    assert_eq!(
        (started.status, started.thing_arns()),
        (200, vec![thing_arn("timer-future-device")]),
        "case \"a started execution\": expected it listed IN_PROGRESS, got {} {:?}",
        started.status,
        started.body
    );
    assert_eq!(
        started.summary_member(0, "startedAt"),
        json!(now),
        "case \"a started execution\": startedAt must be when the device started it, got {:?}",
        started.entries().first()
    );
}
