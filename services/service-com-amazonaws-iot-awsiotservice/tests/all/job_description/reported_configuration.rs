// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the members a reader of an IoT JOB or JOB TEMPLATE always
//! reports: the configuration structures the resource is deployed under, the per-status
//! counters of a job's process details, and whether the job is concurrent.
//!
//! A child of the `job_description` suite — the readers under test are the two that suite
//! drives (`DescribeJob` and `DescribeJobTemplate`), read here for the members they report
//! for EVERY record rather than for the record one create wrote.
//!
//! The rule under test: this service materialises the configuration of every job and job
//! template it keeps, so a reader reports each configuration structure whose own members
//! the model all makes optional — `presignedUrlConfig`, `jobExecutionsRolloutConfig`,
//! `schedulingConfig` and `timeoutConfig` on a job, the first two and `timeoutConfig` on
//! a job template — whether or not anything configured it, with the values the record
//! holds and no member where it holds none; the two structures whose `criteriaList` the
//! model makes `@required` (`abortConfig`, `jobExecutionsRetryConfig`) have no empty form
//! and are reported only from a record that holds criteria. Every member of
//! `JobProcessDetails` other than `processingTargets` is "the number of things" in one
//! `JobExecutionStatus`, so each is a NUMBER — zero for a status no execution of the job
//! is in — `processingTargets` is reported for no job (a create here has finished rolling
//! out to every target before it answers, and the member "will be null after the job
//! execution has finished rolling out to all the target devices"), and `Job$isConcurrent`
//! "will be true when a job is rolling out new job executions or canceling previously
//! created executions, otherwise false", which is every job that has not COMPLETED: one
//! still has executions to roll out or to cancel until all its targets have finished, and a
//! `CONTINUOUS` job — which "will continue to run" — never reaches that point whatever its
//! own status.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `PUT /jobs/{jobId}` (`CreateJob`), `PATCH /jobs/{jobId}` (`UpdateJob`), `GET
//! /jobs/{jobId}` (`DescribeJob`), `PUT /jobs/{jobId}/cancel` (`CancelJob`), `PUT
//! /job-templates/{jobTemplateId}` (`CreateJobTemplate`) and `GET
//! /job-templates/{jobTemplateId}` (`DescribeJobTemplate`), signed for the `iot` signing
//! name — so what is asserted is what a client reads back through the modelled surface.
//!
//! The rows span what the rule generalises over: whether the job was created with a
//! timeout, which `targetSelection` it carries, whether its targets resolve to no device,
//! one or several, whether its executions are still pending or have been cancelled, and —
//! for a template — from no configuration at all to every member the create accepts.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use service_common::iot_job_executions::{JobExecutionRecord, JobExecutionStore, STATUS_QUEUED};
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// The `JobStatus` of a job that is rolling out to its targets.
const JOB_IN_PROGRESS: &str = "IN_PROGRESS";
/// The `JobStatus` of a job whose rollout was cancelled.
const JOB_CANCELED: &str = "CANCELED";
/// The `JobStatus` of a snapshot job every target has finished.
const JOB_COMPLETED: &str = "COMPLETED";

/// The `JobExecutionStatus` a device reports when it has finished the work
/// (`smithy_query(query="shape",shape="JobExecutionStatus")`: QUEUED | IN_PROGRESS |
/// SUCCEEDED | FAILED | TIMED_OUT | REJECTED | REMOVED | CANCELED).
const STATUS_SUCCEEDED: &str = "SUCCEEDED";

/// Every member of `JobProcessDetails` that counts things, from
/// `smithy_query(query="shape",shape="JobProcessDetails")`: each is "the number of
/// things" in one `JobExecutionStatus`. `processingTargets` is the one member the model
/// documents as absent once the rollout is over, so it is not here.
const PROCESS_COUNTERS: [&str; 8] = [
    "numberOfCanceledThings",
    "numberOfFailedThings",
    "numberOfInProgressThings",
    "numberOfQueuedThings",
    "numberOfRejectedThings",
    "numberOfRemovedThings",
    "numberOfSucceededThings",
    "numberOfTimedOutThings",
];

/// The configuration structures of a `Job` whose own members are all optional, from
/// `smithy_query(query="shape",shape="Job")` — the ones a reader of a job reports whether
/// or not anything configured them.
const JOB_CONFIGURATIONS: [&str; 4] = [
    "jobExecutionsRolloutConfig",
    "presignedUrlConfig",
    "schedulingConfig",
    "timeoutConfig",
];

/// The same for a job template, whose description declares no `schedulingConfig`
/// (`smithy_query(query="operation",operation="DescribeJobTemplate")`).
const TEMPLATE_CONFIGURATIONS: [&str; 3] = [
    "jobExecutionsRolloutConfig",
    "presignedUrlConfig",
    "timeoutConfig",
];

/// The two configuration structures whose `criteriaList` the model makes `@required` with
/// `length 1..`, so neither has an empty form: a reader reports one only from a record
/// that holds criteria.
const CRITERIA_CONFIGURATIONS: [&str; 2] = ["abortConfig", "jobExecutionsRetryConfig"];

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name:
/// the scope is how the router picks the claiming service.
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
    body: Value,
}

impl Reply {
    fn text(&self, pointer: &str) -> Option<String> {
        self.body
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The `job` member of a `DescribeJob` answer.
    fn job(&self) -> &Value {
        self.body.get("job").unwrap_or(&Value::Null)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it. The
/// engine handle is kept so a case can put a targeted device's execution in a status only
/// the device-facing plane would otherwise write.
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
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply { status, body }
    }

    async fn create_thing(&self, thing_name: &str) -> Reply {
        self.call("POST", &format!("/things/{thing_name}"), Some(&json!({})))
            .await
    }

    async fn create_thing_group(&self, group_name: &str) -> Reply {
        self.call(
            "POST",
            &format!("/thing-groups/{group_name}"),
            Some(&json!({})),
        )
        .await
    }

    async fn add_thing_to_thing_group(&self, group_name: &str, thing_name: &str) -> Reply {
        self.call(
            "PUT",
            "/thing-groups/addThingToThingGroup",
            Some(&json!({ "thingGroupName": group_name, "thingName": thing_name })),
        )
        .await
    }

    async fn create_job(&self, job_id: &str, request: &Value) -> Reply {
        self.call("PUT", &format!("/jobs/{job_id}"), Some(request))
            .await
    }

    async fn update_job(&self, job_id: &str, request: &Value) -> Reply {
        self.call("PATCH", &format!("/jobs/{job_id}"), Some(request))
            .await
    }

    async fn describe_job(&self, job_id: &str) -> Reply {
        self.call("GET", &format!("/jobs/{job_id}"), None).await
    }

    async fn cancel_job(&self, job_id: &str) -> Reply {
        self.call("PUT", &format!("/jobs/{job_id}/cancel"), Some(&json!({})))
            .await
    }

    async fn create_job_template(&self, job_template_id: &str, request: &Value) -> Reply {
        self.call(
            "PUT",
            &format!("/job-templates/{job_template_id}"),
            Some(request),
        )
        .await
    }

    async fn describe_job_template(&self, job_template_id: &str) -> Reply {
        self.call("GET", &format!("/job-templates/{job_template_id}"), None)
            .await
    }

    /// Put the stored execution of (thing, job) in `status`, the way the device-facing plane
    /// does when a device reports the outcome of the work — the one state a case cannot
    /// reach through this service's own surface.
    fn report_execution(&self, thing_name: &str, job_id: &str, status: &str) {
        let store = self.engine.state::<JobExecutionStore>();
        let mut store = store.lock();
        let index = store
            .execution_index(thing_name, job_id, None)
            .unwrap_or_else(|| panic!("{thing_name} must hold an execution of {job_id}"));
        let record: &mut JobExecutionRecord = store
            .record_mut(thing_name, index)
            .expect("the index came from this store");
        record.status = status.to_string();
    }
}

/// The ARN a `targets` entry names a device by.
fn thing_arn(thing_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing_name}")
}

/// The ARN a `targets` entry names a thing GROUP by.
fn thing_group_arn(group_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/{group_name}")
}

/// Assert that `reported` carries every structure in `configurations` as an object.
fn assert_configurations_reported(case: &str, reported: &Value, configurations: &[&str]) {
    for member in configurations {
        assert!(
            reported
                .get(member)
                .is_some_and(|configuration| configuration.is_object()),
            "case {case:?}: expected {member} to be reported as a structure, got {reported:?}"
        );
    }
}

/// Assert that every counter of `jobProcessDetails` is a number, that
/// `numberOfQueuedThings` is `queued`, that `numberOfCanceledThings` is `canceled` and
/// that every other counter is zero.
fn assert_counters(case: &str, job: &Value, queued: i64, canceled: i64) {
    let details = job
        .get("jobProcessDetails")
        .unwrap_or_else(|| panic!("case {case:?}: expected jobProcessDetails, got {job:?}"));
    assert!(
        details.get("processingTargets").is_none(),
        "case {case:?}: a job that has finished rolling out to every target reports no \
         processingTargets, got {details:?}"
    );
    for member in PROCESS_COUNTERS {
        let expected = match member {
            "numberOfQueuedThings" => queued,
            "numberOfCanceledThings" => canceled,
            _ => 0,
        };
        let actual = details.get(member).and_then(Value::as_i64);
        assert_eq!(
            actual,
            Some(expected),
            "case {case:?}: expected jobProcessDetails.{member} == {expected}, got {actual:?} \
             in {details:?}"
        );
    }
}

/// One row of the job table: the job as created, and what a read of it must then report.
struct JobCase {
    case: &'static str,
    job_id: &'static str,
    /// The devices the request targets by thing ARN.
    devices: &'static [&'static str],
    /// A thing GROUP the request targets by ARN, with the devices to put in it first.
    group: Option<(&'static str, &'static [&'static str])>,
    /// `targetSelection`, or `None` for a request that names none.
    target_selection: Option<&'static str>,
    /// `timeoutConfig.inProgressTimeoutInMinutes`, or `None` for a job created without a
    /// timeout at all.
    timeout_minutes: Option<i64>,
    /// How many executions the targets resolve to — every one of them QUEUED.
    expected_queued: i64,
    expected_is_concurrent: bool,
}

/// The rows differ in `targetSelection`, in the FORM and FAN-OUT of the targets (a group
/// that holds no device, one device, two devices) and in whether the job carries an
/// in-progress timeout.
fn job_cases() -> Vec<JobCase> {
    vec![
        JobCase {
            case: "a CONTINUOUS job whose group target holds no device",
            job_id: "continuous-empty-group",
            devices: &[],
            group: Some(("empty-continuous-group", &[])),
            target_selection: Some("CONTINUOUS"),
            timeout_minutes: None,
            expected_queued: 0,
            // "will continue to run": a continuous job keeps rolling out new executions.
            expected_is_concurrent: true,
        },
        JobCase {
            case: "a SNAPSHOT job whose group target holds no device",
            job_id: "snapshot-empty-group",
            devices: &[],
            group: Some(("empty-snapshot-group", &[])),
            target_selection: Some("SNAPSHOT"),
            timeout_minutes: None,
            expected_queued: 0,
            // Nothing has completed the job, so its rollout is not finished with.
            expected_is_concurrent: true,
        },
        JobCase {
            case: "a CONTINUOUS job targeting two devices with a timeout",
            job_id: "continuous-two-devices",
            devices: &["sensor-a", "sensor-b"],
            group: None,
            target_selection: Some("CONTINUOUS"),
            timeout_minutes: Some(45),
            expected_queued: 2,
            expected_is_concurrent: true,
        },
        JobCase {
            case: "a SNAPSHOT job targeting one device",
            job_id: "snapshot-one-device",
            devices: &["gateway"],
            group: None,
            target_selection: Some("SNAPSHOT"),
            timeout_minutes: Some(1),
            expected_queued: 1,
            // Its one target is still QUEUED, so the job has not completed.
            expected_is_concurrent: true,
        },
        JobCase {
            case: "a job naming no targetSelection, targeting a group of one device",
            job_id: "unselected-group-of-one",
            devices: &[],
            group: Some(("pump-group", &["pump"])),
            target_selection: None,
            timeout_minutes: Some(10_080),
            expected_queued: 1,
            // A job whose target has not finished it has not completed either.
            expected_is_concurrent: true,
        },
    ]
}

/// RULE: a reader of a job reports every configuration structure whose members the model
/// makes optional — empty where the record holds no value — every `jobProcessDetails`
/// counter as a number, and whether the job is concurrent.
#[tokio::test]
async fn a_job_reports_its_configuration_structures_and_its_counters() {
    let emulator = Emulator::new();

    for case in job_cases() {
        let label = case.case;
        let mut targets: Vec<String> = Vec::new();
        for thing_name in case.devices {
            let created = emulator.create_thing(thing_name).await;
            assert_eq!(
                created.status, 200,
                "case {label:?}: the targeted thing must be creatable, got {} {:?}",
                created.status, created.body
            );
            targets.push(thing_arn(thing_name));
        }
        if let Some((group_name, members)) = case.group {
            let created = emulator.create_thing_group(group_name).await;
            assert_eq!(
                created.status, 200,
                "case {label:?}: the targeted group must be creatable, got {} {:?}",
                created.status, created.body
            );
            for thing_name in members {
                let created = emulator.create_thing(thing_name).await;
                assert_eq!(
                    created.status, 200,
                    "case {label:?}: a group member must be creatable, got {} {:?}",
                    created.status, created.body
                );
                let added = emulator
                    .add_thing_to_thing_group(group_name, thing_name)
                    .await;
                assert_eq!(
                    added.status, 200,
                    "case {label:?}: the membership must be writable, got {} {:?}",
                    added.status, added.body
                );
            }
            targets.push(thing_group_arn(group_name));
        }

        let mut request = json!({
            "targets": targets,
            "document": r#"{"operation":"reboot"}"#,
        });
        if let Some(target_selection) = case.target_selection {
            request["targetSelection"] = json!(target_selection);
        }
        if let Some(minutes) = case.timeout_minutes {
            request["timeoutConfig"] = json!({ "inProgressTimeoutInMinutes": minutes });
        }
        let created = emulator.create_job(case.job_id, &request).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: the job must be creatable, got {} {:?}",
            created.status, created.body
        );

        let read = emulator.describe_job(case.job_id).await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: expected 200, got {} {:?}",
            read.status, read.body
        );
        let job = read.job();
        assert_eq!(
            read.text("/job/status").as_deref(),
            Some(JOB_IN_PROGRESS),
            "case {label:?}: expected {JOB_IN_PROGRESS}, got {job:?}"
        );
        assert_configurations_reported(label, job, &JOB_CONFIGURATIONS);
        for member in CRITERIA_CONFIGURATIONS {
            assert!(
                job.get(member).is_none(),
                "case {label:?}: {member} has a required criteriaList, so a job created \
                 without criteria reports none, got {job:?}"
            );
        }
        // The timeout the create named is the one the structure carries; a job created
        // without one carries an EMPTY structure rather than a value nobody set.
        let reported_timeout = job
            .pointer("/timeoutConfig/inProgressTimeoutInMinutes")
            .and_then(Value::as_i64);
        assert_eq!(
            reported_timeout,
            case.timeout_minutes,
            "case {label:?}: expected timeoutConfig to carry {:?}, got {:?}",
            case.timeout_minutes,
            job.get("timeoutConfig")
        );
        // Nothing stages a rollout or schedules a job here, so these two carry no member
        // at all — which is what "no configuration" is reported as.
        for member in ["jobExecutionsRolloutConfig", "schedulingConfig"] {
            assert_eq!(
                job.get(member),
                Some(&json!({})),
                "case {label:?}: expected {member} to carry no member, got {:?}",
                job.get(member)
            );
        }
        assert_counters(label, job, case.expected_queued, 0);
        assert_eq!(
            job.get("isConcurrent").and_then(Value::as_bool),
            Some(case.expected_is_concurrent),
            "case {label:?}: expected isConcurrent == {}, got {:?}",
            case.expected_is_concurrent,
            job.get("isConcurrent")
        );
    }
}

/// RULE: the same members are reported from the job's CURRENT record after its own
/// lifecycle moves — the counters follow the executions into their new status, the
/// configuration structures are still reported, and the description an update wrote is
/// what both the cancel's own answer and the following read report.
#[tokio::test]
async fn a_cancelled_job_reports_the_same_members_from_its_current_record() {
    let emulator = Emulator::new();
    for thing_name in ["probe-a", "probe-b"] {
        let created = emulator.create_thing(thing_name).await;
        assert_eq!(
            created.status, 200,
            "the targeted thing must be creatable, got {} {:?}",
            created.status, created.body
        );
    }
    let created = emulator
        .create_job(
            "cancelled-job",
            &json!({
                "targets": [thing_arn("probe-a"), thing_arn("probe-b")],
                "document": r#"{"operation":"reboot"}"#,
                "targetSelection": "CONTINUOUS",
                "description": "the job as created",
            }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the job must be creatable, got {} {:?}",
        created.status, created.body
    );

    // A member an update writes is the member every later reader reports.
    let updated = emulator
        .update_job(
            "cancelled-job",
            &json!({ "description": "the job as updated" }),
        )
        .await;
    assert_eq!(
        updated.status, 200,
        "case \"an update of the description\": expected 200, got {} {:?}",
        updated.status, updated.body
    );
    let read = emulator.describe_job("cancelled-job").await;
    assert_eq!(
        read.text("/job/description").as_deref(),
        Some("the job as updated"),
        "case \"a read after the update\": expected the updated description, got {:?}",
        read.job()
    );
    assert_counters("a read after the update", read.job(), 2, 0);
    assert_eq!(
        read.job().get("isConcurrent").and_then(Value::as_bool),
        Some(true),
        "case \"a read after the update\": a continuous job that is rolling out is \
         concurrent, got {:?}",
        read.job()
    );

    let cancelled = emulator.cancel_job("cancelled-job").await;
    assert_eq!(
        cancelled.status, 200,
        "case \"the cancel\": expected 200, got {} {:?}",
        cancelled.status, cancelled.body
    );
    assert_eq!(
        cancelled.text("/description").as_deref(),
        Some("the job as updated"),
        "case \"the cancel\": the cancel echoes the job's CURRENT description, got {:?}",
        cancelled.body
    );

    let read = emulator.describe_job("cancelled-job").await;
    assert_eq!(
        read.status, 200,
        "case \"a read after the cancel\": expected 200, got {} {:?}",
        read.status, read.body
    );
    let job = read.job();
    assert_eq!(
        read.text("/job/status").as_deref(),
        Some(JOB_CANCELED),
        "case \"a read after the cancel\": expected {JOB_CANCELED}, got {job:?}"
    );
    assert_configurations_reported("a read after the cancel", job, &JOB_CONFIGURATIONS);
    // The two QUEUED executions the cancel reached are now CANCELED, so the counters
    // report two cancelled things and nothing queued.
    assert_counters("a read after the cancel", job, 0, 2);
    assert_eq!(
        job.get("isConcurrent").and_then(Value::as_bool),
        Some(true),
        "case \"a read after the cancel\": a continuous job has executions still to roll \
         out whatever its own status, got {job:?}"
    );
    assert_eq!(
        read.text("/job/description").as_deref(),
        Some("the job as updated"),
        "case \"a read after the cancel\": expected the updated description, got {job:?}"
    );
}

/// One row of the job-template table: the configuration the create carries, and what the
/// reader must then report for each structure.
struct TemplateCase {
    case: &'static str,
    job_template_id: &'static str,
    /// The optional configuration members the create carries.
    configuration: Value,
    /// Whether the row's configuration names criteria, so the reader reports the two
    /// structures that have no empty form.
    expects_criteria: bool,
}

/// The rows run from a template created with no configuration at all to one carrying
/// every configuration structure the create accepts.
fn template_cases() -> Vec<TemplateCase> {
    vec![
        TemplateCase {
            case: "a template created with no configuration",
            job_template_id: "no-configuration",
            configuration: json!({}),
            expects_criteria: false,
        },
        TemplateCase {
            case: "a template created with a timeout only",
            job_template_id: "timeout-only",
            configuration: json!({ "timeoutConfig": { "inProgressTimeoutInMinutes": 30 } }),
            expects_criteria: false,
        },
        TemplateCase {
            case: "a template created with every configuration structure",
            job_template_id: "every-configuration-structure",
            configuration: json!({
                "presignedUrlConfig": {
                    "roleArn": "arn:aws:iam::000000000000:role/IoTJobPresignedUrlRole",
                    "expiresInSec": 900,
                },
                "jobExecutionsRolloutConfig": { "maximumPerMinute": 10 },
                "timeoutConfig": { "inProgressTimeoutInMinutes": 120 },
                "abortConfig": {
                    "criteriaList": [{
                        "failureType": "FAILED",
                        "action": "CANCEL",
                        "thresholdPercentage": 20.0,
                        "minNumberOfExecutedThings": 5,
                    }],
                },
                "jobExecutionsRetryConfig": {
                    "criteriaList": [{ "failureType": "FAILED", "numberOfRetries": 2 }],
                },
            }),
            expects_criteria: true,
        },
    ]
}

/// RULE: a reader of a job template reports the same configuration structures under the
/// same rule — the stored configuration where the record holds one, an empty structure
/// where it holds none, and the two criteria structures only from a record that holds
/// criteria.
#[tokio::test]
async fn a_job_template_reports_its_configuration_structures() {
    let emulator = Emulator::new();

    for case in template_cases() {
        let label = case.case;
        let mut request = case.configuration.clone();
        {
            let object = request
                .as_object_mut()
                .unwrap_or_else(|| panic!("case {label:?}: the row's configuration is an object"));
            object.insert("description".to_string(), json!("reboot the fleet"));
            object.insert("document".to_string(), json!(r#"{"operation":"reboot"}"#));
        }
        let created = emulator
            .create_job_template(case.job_template_id, &request)
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: the template must be creatable, got {} {:?}",
            created.status, created.body
        );

        let read = emulator.describe_job_template(case.job_template_id).await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: expected 200, got {} {:?}",
            read.status, read.body
        );
        assert_configurations_reported(label, &read.body, &TEMPLATE_CONFIGURATIONS);
        for member in TEMPLATE_CONFIGURATIONS {
            let expected = case.configuration.get(member).cloned().unwrap_or(json!({}));
            assert_eq!(
                read.body.get(member),
                Some(&expected),
                "case {label:?}: expected {member} to read back as {expected}, got {:?}",
                read.body.get(member)
            );
        }
        for member in CRITERIA_CONFIGURATIONS {
            let expected = case.configuration.get(member);
            assert_eq!(
                read.body.get(member),
                expected,
                "case {label:?}: expected {member} to read back as {expected:?}, got {:?}",
                read.body.get(member)
            );
            assert_eq!(
                read.body.get(member).is_some(),
                case.expects_criteria,
                "case {label:?}: {member} is reported exactly when the record holds \
                 criteria, got {:?}",
                read.body.get(member)
            );
        }
    }
}

/// One row of the rollout table: the job as created and left, and what a read of it must
/// then report about its rollout.
struct RolloutCase {
    case: &'static str,
    job_id: &'static str,
    /// `targetSelection`, or `None` for a request that names none.
    target_selection: Option<&'static str>,
    /// The devices the job targets, each with the `JobExecutionStatus` its execution is left
    /// in — `STATUS_QUEUED` for one no device has reported on.
    devices: &'static [(&'static str, &'static str)],
    /// Whether the job's own lifecycle is cancelled before the read.
    cancelled: bool,
    expected_status: &'static str,
    expected_is_concurrent: bool,
    /// What `numberOfQueuedThings` and `numberOfSucceededThings` must report.
    expected_queued: i64,
    expected_succeeded: i64,
}

/// The rows differ in `targetSelection`, in the fan-out of the targets, in how far their
/// executions have got and in whether the job's own lifecycle was cancelled.
fn rollout_cases() -> Vec<RolloutCase> {
    vec![
        RolloutCase {
            case: "a CONTINUOUS job whose one target is still queued",
            job_id: "continuous-queued",
            target_selection: Some("CONTINUOUS"),
            devices: &[("continuous-queued-a", STATUS_QUEUED)],
            cancelled: false,
            expected_status: JOB_IN_PROGRESS,
            expected_is_concurrent: true,
            expected_queued: 1,
            expected_succeeded: 0,
        },
        RolloutCase {
            case: "a SNAPSHOT job whose one target is still queued",
            job_id: "snapshot-queued",
            target_selection: Some("SNAPSHOT"),
            devices: &[("snapshot-queued-a", STATUS_QUEUED)],
            cancelled: false,
            expected_status: JOB_IN_PROGRESS,
            expected_is_concurrent: true,
            expected_queued: 1,
            expected_succeeded: 0,
        },
        RolloutCase {
            case: "a job naming no targetSelection with two queued targets",
            job_id: "unselected-two-queued",
            target_selection: None,
            devices: &[
                ("unselected-two-a", STATUS_QUEUED),
                ("unselected-two-b", STATUS_QUEUED),
            ],
            cancelled: false,
            expected_status: JOB_IN_PROGRESS,
            expected_is_concurrent: true,
            expected_queued: 2,
            expected_succeeded: 0,
        },
        RolloutCase {
            case: "a SNAPSHOT job one of whose two targets has succeeded",
            job_id: "snapshot-part-done",
            target_selection: Some("SNAPSHOT"),
            devices: &[
                ("snapshot-part-a", STATUS_SUCCEEDED),
                ("snapshot-part-b", STATUS_QUEUED),
            ],
            cancelled: false,
            expected_status: JOB_IN_PROGRESS,
            expected_is_concurrent: true,
            expected_queued: 1,
            expected_succeeded: 1,
        },
        RolloutCase {
            case: "a SNAPSHOT job every target of which has succeeded",
            job_id: "snapshot-all-done",
            target_selection: Some("SNAPSHOT"),
            devices: &[
                ("snapshot-done-a", STATUS_SUCCEEDED),
                ("snapshot-done-b", STATUS_SUCCEEDED),
            ],
            cancelled: false,
            // Every target has completed the job, so it is finished with: the one row that
            // is NOT concurrent.
            expected_status: JOB_COMPLETED,
            expected_is_concurrent: false,
            expected_queued: 0,
            expected_succeeded: 2,
        },
        RolloutCase {
            case: "a CONTINUOUS job every target of which has succeeded",
            job_id: "continuous-all-done",
            target_selection: Some("CONTINUOUS"),
            devices: &[("continuous-done-a", STATUS_SUCCEEDED)],
            cancelled: false,
            // It "will continue to run": new executions are still to come, so it never
            // reaches the completed state its current targets would give a snapshot job.
            expected_status: JOB_IN_PROGRESS,
            expected_is_concurrent: true,
            expected_queued: 0,
            expected_succeeded: 1,
        },
        RolloutCase {
            case: "a SNAPSHOT job that was cancelled",
            job_id: "snapshot-cancelled",
            target_selection: Some("SNAPSHOT"),
            devices: &[("snapshot-cancelled-a", STATUS_QUEUED)],
            cancelled: true,
            expected_status: JOB_CANCELED,
            expected_is_concurrent: true,
            // The cancel moved the one QUEUED execution to CANCELED.
            expected_queued: 0,
            expected_succeeded: 0,
        },
        RolloutCase {
            case: "a CONTINUOUS job that was cancelled",
            job_id: "continuous-cancelled",
            target_selection: Some("CONTINUOUS"),
            devices: &[("continuous-cancelled-a", STATUS_QUEUED)],
            cancelled: true,
            expected_status: JOB_CANCELED,
            expected_is_concurrent: true,
            expected_queued: 0,
            expected_succeeded: 0,
        },
    ]
}

/// RULE: a reader of a job reports `isConcurrent` true for every job that has not COMPLETED
/// — one that may still roll out executions or cancel the ones it created — and reports no
/// `processingTargets` for any job, whatever its `targetSelection`, its fan-out, how far its
/// executions have got or whether it was cancelled.
#[tokio::test]
async fn a_job_reports_whether_it_is_still_rolling_out() {
    let emulator = Emulator::new();

    for case in rollout_cases() {
        let label = case.case;
        let mut targets: Vec<String> = Vec::new();
        for (thing_name, _) in case.devices {
            let created = emulator.create_thing(thing_name).await;
            assert_eq!(
                created.status, 200,
                "case {label:?}: the targeted thing must be creatable, got {} {:?}",
                created.status, created.body
            );
            targets.push(thing_arn(thing_name));
        }

        let mut request = json!({
            "targets": targets,
            "document": r#"{"operation":"reboot"}"#,
        });
        if let Some(target_selection) = case.target_selection {
            request["targetSelection"] = json!(target_selection);
        }
        let created = emulator.create_job(case.job_id, &request).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: the job must be creatable, got {} {:?}",
            created.status, created.body
        );
        for (thing_name, status) in case.devices {
            if *status != STATUS_QUEUED {
                emulator.report_execution(thing_name, case.job_id, status);
            }
        }
        if case.cancelled {
            let cancelled = emulator.cancel_job(case.job_id).await;
            assert_eq!(
                cancelled.status, 200,
                "case {label:?}: the cancel must succeed, got {} {:?}",
                cancelled.status, cancelled.body
            );
        }

        let read = emulator.describe_job(case.job_id).await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: expected 200, got {} {:?}",
            read.status, read.body
        );
        let job = read.job();
        assert_eq!(
            job.get("status").and_then(Value::as_str),
            Some(case.expected_status),
            "case {label:?}: expected status {:?}, got {job:?}",
            case.expected_status
        );
        assert_eq!(
            job.get("isConcurrent").and_then(Value::as_bool),
            Some(case.expected_is_concurrent),
            "case {label:?}: expected isConcurrent == {}, got {:?}",
            case.expected_is_concurrent,
            job.get("isConcurrent")
        );
        let details = job
            .get("jobProcessDetails")
            .unwrap_or_else(|| panic!("case {label:?}: expected jobProcessDetails, got {job:?}"));
        assert!(
            details.get("processingTargets").is_none(),
            "case {label:?}: a job that has finished rolling out reports no \
             processingTargets, got {details:?}"
        );
        // The counters still report what the executions say, so the two members above are
        // the only thing these rules change.
        for (member, expected) in [
            ("numberOfQueuedThings", case.expected_queued),
            ("numberOfSucceededThings", case.expected_succeeded),
        ] {
            let actual = details.get(member).and_then(Value::as_i64);
            assert_eq!(
                actual,
                Some(expected),
                "case {label:?}: expected jobProcessDetails.{member} == {expected}, got \
                 {actual:?} in {details:?}"
            );
        }
    }
}
