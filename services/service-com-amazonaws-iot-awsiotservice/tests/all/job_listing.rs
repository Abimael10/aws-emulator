// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `ListJobs`: the control plane lists the account's jobs out
//! of the SHARED jobs store (`service_common::iot_jobs`) every creator of an IoT job
//! records one in, each projected onto `JobSummary`.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `GET /jobs` signed for the `iot` signing name — after creating the jobs through
//! `PUT /jobs/{jobId}`, their groups through `POST /thing-groups/{thingGroupName}` and
//! their cancellations through `PUT /jobs/{jobId}/cancel`, so what is listed is what a
//! writer persisted.
//!
//! The rules under test:
//!
//! * the listing reports one entry per job the store holds, each the stored record
//!   projected onto `JobSummary` — `jobArn`, `jobId`, `thingGroupId`, `targetSelection`,
//!   `status`, `createdAt`, `lastUpdatedAt`, `completedAt` and `isConcurrent` — in the
//!   store's `jobId` order, agreeing member for member with `DescribeJob`, and reading it
//!   changes no job;
//! * `status` selects the jobs the listing reports in that status — the status its targets
//!   have made of it and not only the one its own record carries — and a status no job is
//!   in is an EMPTY list, not a refusal;
//! * `targetSelection` selects the jobs created with that selection, and a job whose
//!   creator named none is selected by neither value;
//! * `thingGroupName` and `thingGroupId` each limit the listing to the jobs whose targets
//!   name that group, and a group no job targets — or that the registry does not hold at
//!   all — selects none;
//! * `maxResults` and `nextToken` walk that one order, every job exactly once, and a
//!   `nextToken` that is not a token of THIS listing is refused 400
//!   `InvalidRequestException`.
//!
//! Each table spans the class a rule generalises over — every `JobStatus` the model
//! declares, both `TargetSelection` values and the absent one, a job targeting a thing, one
//! group, two groups, each published `maxResults` bound and one past it — rather than one
//! recorded request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use service_common::iot_job_executions::{now_epoch_seconds, JobExecutionRecord, JobExecutionStore};
use service_common::iot_jobs::{IotJob, IotJobStore};
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// The values `JobStatus` admits
/// (`smithy_query(query="shape",shape="JobStatus")`), which is the catalog the `status`
/// filter is driven over.
const JOB_STATUSES: &[&str] = &[
    "CANCELED",
    "COMPLETED",
    "DELETION_IN_PROGRESS",
    "IN_PROGRESS",
    "SCHEDULED",
];

/// The values `TargetSelection` admits
/// (`smithy_query(query="shape",shape="TargetSelection")`).
const TARGET_SELECTIONS: &[&str] = &["CONTINUOUS", "SNAPSHOT"];

/// The largest page `LaserMaxResults` admits (`range 1..250`).
const MAX_RESULTS_CEILING: i32 = 250;

/// A `thingGroupId` the registry holds no group under: a canonical hyphenated UUID, the
/// form a minted `thingGroupId` takes, so the filter is driven with a well-formed id and
/// not with a string the model would refuse.
const UNKNOWN_GROUP_ID: &str = "00000000-0000-4000-8000-000000000000";

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
    /// The listed summaries, or an empty slice when the member is absent.
    fn jobs(&self) -> &[Value] {
        self.body["jobs"].as_array().map_or(&[][..], Vec::as_slice)
    }

    /// The listed `jobId`s, in the order they were reported.
    fn job_ids(&self) -> Vec<String> {
        self.jobs()
            .iter()
            .map(|summary| summary["jobId"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// The summary of one listed job, or `None` when the listing does not carry it.
    fn summary_of(&self, job_id: &str) -> Option<&Value> {
        self.jobs()
            .iter()
            .find(|summary| summary["jobId"].as_str() == Some(job_id))
    }

    fn next_token(&self) -> Option<String> {
        self.body
            .get("nextToken")
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it. The
/// engine handle is kept so a case can move a stored execution the way the device-facing
/// plane moves it and then read the job back through this service.
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
            // restJson1 may qualify the header with the shape's documentation URI
            // (`<code>:<uri>`); the CODE is what a client branches on.
            .map(|value| {
                value
                    .split_once(':')
                    .map_or(value, |(code, _)| code)
                    .to_string()
            });
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

    async fn create_thing(&self, thing_name: &str) {
        let created = self
            .call("POST", &format!("/things/{thing_name}"), Some(&json!({})))
            .await;
        assert_eq!(
            created.status, 200,
            "creating thing {thing_name:?} must answer 200, got {} {:?}",
            created.status, created.body
        );
    }

    /// `CreateThingGroup`, answering the `thingGroupId` the service minted for the group —
    /// the id a `thingGroupId` filter names it by.
    async fn create_thing_group(&self, thing_group_name: &str) -> String {
        let created = self
            .call(
                "POST",
                &format!("/thing-groups/{thing_group_name}"),
                Some(&json!({})),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "creating thing group {thing_group_name:?} must answer 200, got {} {:?}",
            created.status, created.body
        );
        created.body["thingGroupId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("creating thing group {thing_group_name:?} must report a thingGroupId")
            })
            .to_string()
    }

    async fn add_thing_to_thing_group(&self, thing_group_name: &str, thing_name: &str) {
        let added = self
            .call(
                "PUT",
                "/thing-groups/addThingToThingGroup",
                Some(&json!({ "thingGroupName": thing_group_name, "thingName": thing_name })),
            )
            .await;
        assert_eq!(
            added.status, 200,
            "adding {thing_name:?} to {thing_group_name:?} must answer 200, got {} {:?}",
            added.status, added.body
        );
    }

    /// `CreateJob` targeting `targets` verbatim, optionally with a `targetSelection`.
    async fn create_job(&self, job_id: &str, targets: &[String], target_selection: Option<&str>) {
        let mut request = json!({
            "targets": targets,
            "document": format!(r#"{{"operation":"reboot","job":"{job_id}"}}"#),
        });
        if let Some(target_selection) = target_selection {
            request["targetSelection"] = json!(target_selection);
        }
        let created = self
            .call("PUT", &format!("/jobs/{job_id}"), Some(&request))
            .await;
        assert_eq!(
            created.status, 200,
            "creating job {job_id:?} must answer 200, got {} {:?}",
            created.status, created.body
        );
    }

    /// `CancelJob`: the call that moves a job's own lifecycle to `CANCELED`.
    async fn cancel_job(&self, job_id: &str) {
        let cancelled = self
            .call("PUT", &format!("/jobs/{job_id}/cancel"), Some(&json!({})))
            .await;
        assert_eq!(
            cancelled.status, 200,
            "cancelling job {job_id:?} must answer 200, got {} {:?}",
            cancelled.status, cancelled.body
        );
    }

    async fn list_with_query(&self, query: &str) -> Reply {
        self.call("GET", &format!("/jobs{query}"), None).await
    }

    async fn list(&self) -> Reply {
        self.list_with_query("").await
    }

    /// `DescribeJob`: the single-job reader a listed summary must agree with.
    async fn describe(&self, job_id: &str) -> Reply {
        self.call("GET", &format!("/jobs/{job_id}"), None).await
    }

    /// Mutate the stored execution of (thing, job) in the SHARED store, so a case can put a
    /// record in a state only the device-facing plane reaches — a terminal status, an armed
    /// timer — and then read the job back through this service.
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

    /// The status the SHARED store holds for (thing, job) — what the device polling the Jobs
    /// data plane would see.
    fn stored_execution_status(&self, thing_name: &str, job_id: &str) -> String {
        let store = self.engine.state::<JobExecutionStore>();
        let store = store.lock();
        store
            .execution_index(thing_name, job_id, None)
            .and_then(|index| store.record(thing_name, index))
            .map(|record| record.status.clone())
            .unwrap_or_default()
    }

    /// Record `count` jobs in the SHARED jobs store, as the record every creator of an IoT
    /// job mints one (`IotJob::in_progress`), and answer their ids in store order.
    ///
    /// The bulk seeding path: the bounds a single call is held to are about how many jobs
    /// the store holds, so a case that drives them needs more jobs than a create-per-call
    /// loop is worth. The ids are zero-padded so their string order is their numeric one.
    fn seed_jobs(&self, prefix: &str, count: usize) -> Vec<String> {
        let store = self.engine.state::<IotJobStore>();
        let mut store = store.lock();
        let mut job_ids = Vec::with_capacity(count);
        for index in 0..count {
            let job_id = format!("{prefix}-{index:05}");
            store
                .register(IotJob::in_progress(
                    job_id.clone(),
                    job_arn(&job_id),
                    now_epoch_seconds(),
                ))
                .unwrap_or_else(|refusal| panic!("seeding {job_id}: {refusal}"));
            job_ids.push(job_id);
        }
        job_ids
    }
}

/// The ARN of a thing: what a `targets` entry names a device by.
fn thing_arn(thing_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing_name}")
}

/// The ARN of a thing group: what a `targets` entry names a group by.
fn thing_group_arn(thing_group_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/{thing_group_name}")
}

/// The ARN a created job carries, which is what the listing reports it under.
fn job_arn(job_id: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:job/{job_id}")
}

/// One row of the projection table: a job, what it targets and the selection it was
/// created with.
struct JobCase {
    case: &'static str,
    job_id: &'static str,
    things: &'static [&'static str],
    target_selection: Option<&'static str>,
}

/// RULE: the listing reports every job the store holds, each the stored record projected
/// onto `JobSummary`, in the store's `jobId` order — agreeing with `DescribeJob` member for
/// member — and reading it changes no job.
#[tokio::test]
async fn every_job_the_store_holds_is_listed_as_a_summary() {
    let emulator = Emulator::new();

    // The rows differ in how many devices one job targets, in whether the create order
    // agrees with the ids' order (so the listing's order is the store's and not the create
    // order), in the `targetSelection` the job carries, and in whether two jobs share a
    // device.
    let jobs: &[JobCase] = &[
        JobCase {
            case: "one device, no selection",
            job_id: "listall-c",
            things: &["listall-m"],
            target_selection: None,
        },
        JobCase {
            case: "three devices, CONTINUOUS",
            job_id: "listall-a",
            things: &["listall-z", "listall-a", "listall-m"],
            target_selection: Some("CONTINUOUS"),
        },
        JobCase {
            case: "a device another job already targets, SNAPSHOT",
            job_id: "listall-b",
            things: &["listall-m"],
            target_selection: Some("SNAPSHOT"),
        },
    ];

    for thing in ["listall-a", "listall-m", "listall-z"] {
        emulator.create_thing(thing).await;
    }
    for row in jobs {
        let targets: Vec<String> = row.things.iter().map(|thing| thing_arn(thing)).collect();
        emulator
            .create_job(row.job_id, &targets, row.target_selection)
            .await;
    }

    let listed = emulator.list().await;
    assert_eq!(
        listed.status, 200,
        "the listing must answer 200, got {} {:?}",
        listed.status, listed.body
    );
    assert_eq!(
        listed.job_ids(),
        vec!["listall-a", "listall-b", "listall-c"],
        "case \"three jobs created out of id order\": expected the store's jobId order, \
         got {:?}",
        listed.body
    );

    for row in jobs {
        let summary = listed
            .summary_of(row.job_id)
            .unwrap_or_else(|| panic!("case {:?}: {} must be listed", row.case, row.job_id))
            .clone();
        assert_eq!(
            summary["jobArn"].as_str(),
            Some(job_arn(row.job_id).as_str()),
            "case {:?}: expected the ARN the create reported, got {summary:?}",
            row.case
        );
        assert_eq!(
            summary["status"].as_str(),
            Some("IN_PROGRESS"),
            "case {:?}: a job rolling out to its targets is IN_PROGRESS, got {summary:?}",
            row.case
        );
        assert_eq!(
            summary.get("targetSelection").and_then(Value::as_str),
            row.target_selection,
            "case {:?}: expected the selection the create named, got {summary:?}",
            row.case
        );
        assert_eq!(
            summary["isConcurrent"].as_bool(),
            Some(true),
            "case {:?}: a job that is not COMPLETED still has executions to roll out, \
             got {summary:?}",
            row.case
        );
        assert!(
            summary.get("completedAt").is_none(),
            "case {:?}: a job no target has finished reports no completedAt, got {summary:?}",
            row.case
        );
        assert!(
            summary.get("thingGroupId").is_none(),
            "case {:?}: a job that targets no thing group reports no thingGroupId, \
             got {summary:?}",
            row.case
        );
        let created_at = summary
            .get("createdAt")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("case {:?}: expected a createdAt, got {summary:?}", row.case));
        assert_eq!(
            summary.get("lastUpdatedAt").and_then(Value::as_f64),
            Some(created_at),
            "case {:?}: nothing has updated the job since its create, got {summary:?}",
            row.case
        );

        // The listing and the single-job reader are one projection of one record.
        let described = emulator.describe(row.job_id).await;
        for member in [
            "jobArn",
            "jobId",
            "targetSelection",
            "status",
            "createdAt",
            "lastUpdatedAt",
            "isConcurrent",
        ] {
            assert_eq!(
                summary.get(member),
                described.body.pointer(&format!("/job/{member}")),
                "case {:?}/{member}: expected the listing and the describe to agree, \
                 got {:?} and {:?}",
                row.case,
                summary.get(member),
                described.body.pointer(&format!("/job/{member}"))
            );
        }
    }

    // A listing is a READ: the second one reports exactly what the first did, and every
    // device's execution is still where its create left it.
    let again = emulator.list().await;
    assert_eq!(
        again.body, listed.body,
        "case \"the same listing twice\": a read changes no job, got {:?} then {:?}",
        listed.body, again.body
    );
    for thing in ["listall-a", "listall-m", "listall-z"] {
        let status = emulator.stored_execution_status(thing, "listall-a");
        assert_eq!(
            status, "QUEUED",
            "case \"the same listing twice\": {thing}'s execution must still be QUEUED, \
             got {status:?}"
        );
    }

    // `namespaceId` names the MQTT topic namespace a job's notifications are published
    // under — "Amazon Web Services IoT Core sends jobs notifications to MQTT topics that
    // contain the value" — and not a filter over the account's jobs, so it selects no
    // subset of them.
    let with_namespace = emulator.list_with_query("?namespaceId=greengrass").await;
    assert_eq!(
        (with_namespace.status, with_namespace.job_ids()),
        (200, listed.job_ids()),
        "case \"a namespaceId\": expected every job, got {} {:?}",
        with_namespace.status,
        with_namespace.body
    );
}

/// RULE: `status` selects the jobs the listing reports in that status — the status the job's
/// targets have made of it, not only the one its own record carries — and a status no job is
/// in is an empty list.
#[tokio::test]
async fn the_status_filter_selects_the_status_each_job_is_in() {
    let emulator = Emulator::new();
    let thing = "status-device";
    emulator.create_thing(thing).await;

    // One job per status a job here can be read in: rolling out, cancelled through the
    // control plane, and a SNAPSHOT job whose only target has finished it — which is
    // COMPLETED although its own record still says otherwise.
    emulator
        .create_job("status-progress", &[thing_arn(thing)], Some("CONTINUOUS"))
        .await;
    emulator
        .create_job("status-cancelled", &[thing_arn(thing)], None)
        .await;
    emulator
        .create_job("status-complete", &[thing_arn(thing)], Some("SNAPSHOT"))
        .await;
    emulator.cancel_job("status-cancelled").await;
    let finished_at = now_epoch_seconds();
    emulator.with_stored_execution(thing, "status-complete", |record| {
        record.status = "SUCCEEDED".to_string();
        record.last_updated_at = finished_at;
    });

    // (the modelled status, the jobs it must select)
    let expected: &[(&str, Vec<&str>)] = &[
        ("CANCELED", vec!["status-cancelled"]),
        ("COMPLETED", vec!["status-complete"]),
        ("DELETION_IN_PROGRESS", Vec::new()),
        ("IN_PROGRESS", vec!["status-progress"]),
        ("SCHEDULED", Vec::new()),
    ];
    assert_eq!(
        expected.len(),
        JOB_STATUSES.len(),
        "the table must cover every modelled JobStatus, got {:?} of {JOB_STATUSES:?}",
        expected.iter().map(|(status, _)| *status).collect::<Vec<_>>()
    );

    for (status, wanted) in expected {
        let listed = emulator.list_with_query(&format!("?status={status}")).await;
        assert_eq!(
            (listed.status, listed.job_ids()),
            (
                200,
                wanted.iter().map(|id| (*id).to_string()).collect::<Vec<_>>()
            ),
            "case {status:?}: expected {wanted:?}, got {} {:?}",
            listed.status,
            listed.body
        );
    }

    // No filter reports all three, and the completed job carries the instant its last
    // target finished and is no longer concurrent.
    let listed = emulator.list().await;
    assert_eq!(
        listed.job_ids(),
        vec!["status-cancelled", "status-complete", "status-progress"],
        "case \"no status filter\": expected every job, got {:?}",
        listed.body
    );
    let completed = listed
        .summary_of("status-complete")
        .expect("the completed job is listed")
        .clone();
    assert_eq!(
        completed.get("completedAt").and_then(Value::as_f64),
        Some(finished_at as f64),
        "case \"a snapshot job every target finished\": expected completedAt at the \
         instant its execution last moved, got {completed:?}"
    );
    assert_eq!(
        completed["isConcurrent"].as_bool(),
        Some(false),
        "case \"a snapshot job every target finished\": a COMPLETED job rolls out nothing \
         further, got {completed:?}"
    );
}

/// RULE: `targetSelection` selects the jobs created with that selection, and a job whose
/// creator named none is selected by neither value.
#[tokio::test]
async fn the_target_selection_filter_selects_the_jobs_created_with_it() {
    let emulator = Emulator::new();
    let thing = "selection-device";
    emulator.create_thing(thing).await;

    // (jobId, the selection the create named)
    let jobs: &[(&str, Option<&str>)] = &[
        ("selection-continuous", Some("CONTINUOUS")),
        ("selection-snapshot", Some("SNAPSHOT")),
        ("selection-none", None),
    ];
    for (job_id, target_selection) in jobs {
        emulator
            .create_job(job_id, &[thing_arn(thing)], *target_selection)
            .await;
    }

    for selection in TARGET_SELECTIONS {
        let listed = emulator
            .list_with_query(&format!("?targetSelection={selection}"))
            .await;
        let wanted: Vec<String> = jobs
            .iter()
            .filter(|(_, created_with)| *created_with == Some(*selection))
            .map(|(job_id, _)| (*job_id).to_string())
            .collect();
        assert_eq!(
            (listed.status, listed.job_ids()),
            (200, wanted.clone()),
            "case {selection:?}: expected {wanted:?}, got {} {:?}",
            listed.status,
            listed.body
        );
    }

    let listed = emulator.list().await;
    assert_eq!(
        listed.job_ids(),
        vec![
            "selection-continuous",
            "selection-none",
            "selection-snapshot"
        ],
        "case \"no selection filter\": expected every job, got {:?}",
        listed.body
    );
}

/// RULE: a group filter limits the listing to the jobs whose targets name that group — by
/// the group's name or by the id the registry minted for it — and the summary of such a job
/// reports that id.
#[tokio::test]
async fn the_group_filters_select_the_jobs_that_target_that_group() {
    let emulator = Emulator::new();
    let thing = "group-device";
    emulator.create_thing(thing).await;

    let fleet_a = "listjobs-fleet-a";
    let fleet_b = "listjobs-fleet-b";
    let idle = "listjobs-fleet-idle";
    let id_a = emulator.create_thing_group(fleet_a).await;
    let id_b = emulator.create_thing_group(fleet_b).await;
    emulator.create_thing_group(idle).await;
    emulator.add_thing_to_thing_group(fleet_a, thing).await;

    // The rows differ in what a job targets: one group, the other group, both groups in
    // order, and a device only.
    emulator
        .create_job("group-a", &[thing_group_arn(fleet_a)], None)
        .await;
    emulator
        .create_job("group-b", &[thing_group_arn(fleet_b)], None)
        .await;
    emulator
        .create_job(
            "group-both",
            &[thing_group_arn(fleet_a), thing_group_arn(fleet_b)],
            None,
        )
        .await;
    emulator
        .create_job("group-none", &[thing_arn(thing)], None)
        .await;

    // (case, query, the jobs it must report)
    let rows: &[(&str, String, Vec<&str>)] = &[
        (
            "the name of one group",
            format!("?thingGroupName={fleet_a}"),
            vec!["group-a", "group-both"],
        ),
        (
            "the id of that group",
            format!("?thingGroupId={id_a}"),
            vec!["group-a", "group-both"],
        ),
        (
            "the name of the other group",
            format!("?thingGroupName={fleet_b}"),
            vec!["group-b", "group-both"],
        ),
        (
            "a name and the id of the same group",
            format!("?thingGroupName={fleet_a}&thingGroupId={id_a}"),
            vec!["group-a", "group-both"],
        ),
        (
            "a name and the id of different groups",
            format!("?thingGroupName={fleet_a}&thingGroupId={id_b}"),
            vec!["group-both"],
        ),
        (
            "a group no job targets",
            format!("?thingGroupName={idle}"),
            Vec::new(),
        ),
        (
            "a group name the registry does not hold",
            "?thingGroupName=listjobs-fleet-absent".to_string(),
            Vec::new(),
        ),
        (
            "an id the registry does not hold",
            format!("?thingGroupId={UNKNOWN_GROUP_ID}"),
            Vec::new(),
        ),
        (
            "no group filter",
            String::new(),
            vec!["group-a", "group-b", "group-both", "group-none"],
        ),
    ];

    for (case, query, wanted) in rows {
        let listed = emulator.list_with_query(query).await;
        assert_eq!(
            (listed.status, listed.job_ids()),
            (
                200,
                wanted.iter().map(|id| (*id).to_string()).collect::<Vec<_>>()
            ),
            "case {case:?}: expected {wanted:?}, got {} {:?}",
            listed.status,
            listed.body
        );
    }

    // `JobSummary$thingGroupId` is the id of the group the job targets, read from the
    // registry that minted it.
    let listed = emulator.list().await;
    for (case, job_id, expected) in [
        ("one group", "group-a", Some(id_a.as_str())),
        ("the other group", "group-b", Some(id_b.as_str())),
        ("the first of two groups", "group-both", Some(id_a.as_str())),
        ("a device only", "group-none", None),
    ] {
        let summary = listed
            .summary_of(job_id)
            .unwrap_or_else(|| panic!("case {case:?}: {job_id} must be listed"))
            .clone();
        assert_eq!(
            summary.get("thingGroupId").and_then(Value::as_str),
            expected,
            "case {case:?}: expected thingGroupId {expected:?}, got {summary:?}"
        );
    }
}

/// The most jobs one call may EXAMINE, however selective its filters are
/// (`MAX_SCANNED_JOBS` in `ops/list_jobs.rs`): ten pages' worth.
const MAX_SCANNED_JOBS: usize = MAX_RESULTS_CEILING as usize * 10;

/// RULE: one call answers at most one page and examines at most a bounded number of jobs,
/// so neither an absent `maxResults` nor a filter almost nothing matches makes a single
/// request do work proportional to the whole store — a call that stops on either bound
/// comes back with a `nextToken`, and following the tokens walks every job exactly once.
#[tokio::test]
async fn one_call_is_bounded_in_what_it_returns_and_what_it_examines() {
    let emulator = Emulator::new();
    // More jobs than both bounds admit in one call, so each bound is observed being hit
    // and then passed by following the token.
    let job_ids = emulator.seed_jobs("bulk", MAX_SCANNED_JOBS + 100);

    // A request that names no `maxResults` is answered with the largest page the member
    // admits, and says the listing continues.
    let first = emulator.list().await;
    assert_eq!(
        (first.status, first.jobs().len()),
        (200, MAX_RESULTS_CEILING as usize),
        "case \"no maxResults\": expected a full page of {MAX_RESULTS_CEILING}, got {} with \
         {} summaries",
        first.status,
        first.jobs().len()
    );
    assert_eq!(
        first.job_ids(),
        job_ids[..MAX_RESULTS_CEILING as usize].to_vec(),
        "case \"no maxResults\": expected the first page in id order, got {:?}",
        first.job_ids()
    );
    assert!(
        first.next_token().is_some(),
        "case \"no maxResults\": a listing with jobs behind it must continue, got {:?}",
        first.body
    );

    // Following those pages walks every seeded job exactly once and terminates.
    let mut walked: Vec<String> = Vec::new();
    let mut token = first.next_token();
    walked.extend(first.job_ids());
    let mut pages = 1;
    while let Some(cursor) = token {
        let listed = emulator
            .list_with_query(&format!("?nextToken={cursor}"))
            .await;
        assert_eq!(
            listed.status, 200,
            "case \"following the default pages\": page {pages} must answer 200, got {} {:?}",
            listed.status, listed.body
        );
        walked.extend(listed.job_ids());
        pages += 1;
        token = listed.next_token();
        assert!(
            pages <= job_ids.len(),
            "case \"following the default pages\": the walk must end, got {pages} pages"
        );
    }
    assert_eq!(
        walked, job_ids,
        "case \"following the default pages\": expected every job exactly once in id order, \
         got {} ids",
        walked.len()
    );

    // A filter no job matches does NOT walk the whole store in one call: the first call
    // stops on the scan budget with an empty page and a token, and the next one finishes
    // the walk and stops.
    let filtered = emulator
        .list_with_query("?status=DELETION_IN_PROGRESS")
        .await;
    assert_eq!(
        (filtered.status, filtered.jobs().len()),
        (200, 0),
        "case \"a filter nothing matches\": expected an empty page, got {} {:?}",
        filtered.status,
        filtered.body
    );
    let budget_token = filtered.next_token().unwrap_or_else(|| {
        panic!(
            "case \"a filter nothing matches\": a call that stopped on the scan budget must \
             continue, got {:?}",
            filtered.body
        )
    });
    let rest = emulator
        .list_with_query(&format!(
            "?status=DELETION_IN_PROGRESS&nextToken={budget_token}"
        ))
        .await;
    assert_eq!(
        (rest.status, rest.jobs().len(), rest.next_token()),
        (200, 0, None),
        "case \"a filter nothing matches, resumed\": expected the walk to end empty, \
         got {} {:?}",
        rest.status,
        rest.body
    );
}

#[tokio::test]
async fn pages_walk_every_job_exactly_once() {
    let emulator = Emulator::new();
    let thing = "paging-device";
    emulator.create_thing(thing).await;

    let job_ids: Vec<String> = (0..7).map(|index| format!("paging-{index}")).collect();
    for job_id in &job_ids {
        emulator
            .create_job(job_id, &[thing_arn(thing)], None)
            .await;
    }

    // Page sizes spanning one job per page, sizes that do not divide the total, the exact
    // total, and the published maximum of `LaserMaxResults` (`range 1..250`).
    for page_size in [1, 2, 3, 7, MAX_RESULTS_CEILING] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let query = match token.as_deref() {
                Some(token) => format!("?maxResults={page_size}&nextToken={token}"),
                None => format!("?maxResults={page_size}"),
            };
            let listed = emulator.list_with_query(&query).await;
            assert_eq!(
                listed.status, 200,
                "case maxResults={page_size}: page {pages} must answer 200, got {} {:?}",
                listed.status, listed.body
            );
            assert!(
                listed.jobs().len() <= page_size as usize,
                "case maxResults={page_size}: page {pages} carries {} summaries, got {:?}",
                listed.jobs().len(),
                listed.body
            );
            walked.extend(listed.job_ids());
            pages += 1;
            token = listed.next_token();
            if token.is_none() {
                break;
            }
            assert!(
                pages <= job_ids.len(),
                "case maxResults={page_size}: the walk must end, got {walked:?}"
            );
        }
        assert_eq!(
            walked, job_ids,
            "case maxResults={page_size}: expected every job exactly once in id order, \
             got {walked:?}"
        );
    }

    // Each `LaserMaxResults` bound, and one past each: the range is modelled, so a page size
    // outside it is refused before this listing runs.
    for (case, page_size, expected) in [
        ("the lower bound", 1, 200),
        ("one below the lower bound", 0, 400),
        ("the upper bound", MAX_RESULTS_CEILING, 200),
        ("one past the upper bound", MAX_RESULTS_CEILING + 1, 400),
    ] {
        let listed = emulator
            .list_with_query(&format!("?maxResults={page_size}"))
            .await;
        assert_eq!(
            listed.status, expected,
            "case {case:?}: maxResults={page_size} must answer {expected}, got {} {:?}",
            listed.status, listed.body
        );
    }

    // A token of ANOTHER listing — one with a filter this request does not carry — resumes
    // no page here, and neither does a string this listing never issued.
    let filtered = emulator
        .list_with_query("?status=IN_PROGRESS&maxResults=1")
        .await;
    let filtered_token = filtered
        .next_token()
        .expect("a filtered listing of seven jobs continues");
    for (case, token) in [
        ("a token of a filtered listing", filtered_token.clone()),
        ("a string that is not hex", "not-a-token".to_string()),
        ("hex that carries no position", "6e6f6e65".to_string()),
    ] {
        let listed = emulator
            .list_with_query(&format!("?maxResults=1&nextToken={token}"))
            .await;
        assert_eq!(
            (listed.status, listed.error_code.as_deref()),
            (400, Some("InvalidRequestException")),
            "case {case:?}: expected 400 InvalidRequestException, got {} {:?} with {:?}",
            listed.status,
            listed.error_code,
            listed.body
        );
    }

    // That same token DOES resume the listing it was issued for.
    let resumed = emulator
        .list_with_query(&format!(
            "?status=IN_PROGRESS&maxResults=1&nextToken={filtered_token}"
        ))
        .await;
    assert_eq!(
        (resumed.status, resumed.job_ids()),
        (200, vec!["paging-1".to_string()]),
        "case \"the token of its own listing\": expected the second job, got {} {:?}",
        resumed.status,
        resumed.body
    );
}
