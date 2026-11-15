// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `UpdateJob`: what a partial update writes onto the job in
//! the SHARED jobs store (`service_common::iot_jobs`) is what every later reader of that
//! job reports.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `PUT /jobs/{jobId}` (`CreateJob`), `PATCH /jobs/{jobId}` (`UpdateJob`), `GET
//! /jobs/{jobId}` (`DescribeJob`) and `PUT /jobs/{jobId}/cancel` (`CancelJob`), signed for
//! the `iot` signing name — so what is asserted is what a client reads back through the
//! modelled surface.
//!
//! The rules under test:
//!
//! * a write accepted by `UpdateJob` is reflected by every later read of that job: the
//!   members it names are stored on the record, a member it omits names no change, and
//!   both `DescribeJob` and the identity echo `CancelJob` answers report the CURRENT
//!   values rather than the ones the job was created with;
//! * `timeoutConfig.inProgressTimeoutInMinutes` is "the amount of time each device has to
//!   finish its execution of the job" whose "timer is started when the job execution
//!   status is set to IN_PROGRESS", so an update of it is carried onto the job's
//!   executions that no device has started yet;
//! * a `jobId` no record holds is the modelled `ResourceNotFoundException` (HTTP 404) and
//!   that refusal writes nothing.
//!
//! The table spans what the rules generalise over — whether the job was created with a
//! description or a timeout, which of the two members the update names, whether it
//! replaces, sets or clears a value, and an update that names neither — rather than one
//! recorded request. The last test walks the shape of the recorded flow a cancel of a
//! group-targeted `CONTINUOUS` job takes, over both target forms a job accepts.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use service_common::iot_job_executions::JobExecutionStore;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// The `JobStatus` of a job that is rolling out to its targets.
const JOB_IN_PROGRESS: &str = "IN_PROGRESS";
/// The `JobStatus` of a job whose rollout was cancelled.
const JOB_CANCELED: &str = "CANCELED";

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
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    fn text(&self, pointer: &str) -> Option<String> {
        self.body
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn message(&self) -> String {
        self.text("/message").unwrap_or_default()
    }

    /// The `job` member of a `DescribeJob` answer.
    fn job(&self) -> &Value {
        self.body.get("job").unwrap_or(&Value::Null)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it. The
/// engine handle is kept so a case can read the SHARED job-execution store the data plane
/// serves.
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

    async fn create_thing_group(&self, group_name: &str) -> Reply {
        self.call("POST", &format!("/thing-groups/{group_name}"), Some(&json!({})))
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

    /// The in-progress timeout the stored execution of (thing, job) will arm its timer
    /// from — the device-facing record in the SHARED store, read rather than written.
    fn stored_execution_timeout(&self, thing_name: &str, job_id: &str) -> Option<i64> {
        let store = self.engine.state::<JobExecutionStore>();
        let store = store.lock();
        let index = store
            .execution_index(thing_name, job_id, None)
            .unwrap_or_else(|| panic!("{thing_name} must hold an execution of {job_id}"));
        store
            .record(thing_name, index)
            .expect("the index came from this store")
            .in_progress_timeout_minutes
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

/// The job ARN this plane mints for `job_id` — `Job$jobArn` is "an ARN identifying the
/// job with format arn:aws:iot:region:account:job/jobId".
fn job_arn(job_id: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:job/{job_id}")
}

/// One row of the update table: the job as created, the members the update names, and
/// what every later read of the job must then report.
struct UpdateCase {
    case: &'static str,
    job_id: &'static str,
    thing_name: &'static str,
    created_description: Option<&'static str>,
    created_timeout_minutes: Option<i64>,
    /// `Some(text)` names `description`; `None` leaves it out of the request.
    update_description: Option<&'static str>,
    /// `Some(Some(minutes))` names `timeoutConfig` with a deadline, `Some(None)` names
    /// the structure with no deadline in it, `None` leaves `timeoutConfig` out.
    update_timeout_minutes: Option<Option<i64>>,
    expected_description: Option<&'static str>,
    expected_timeout_minutes: Option<i64>,
}

/// Create the row's job with one targeted device, as a creator records it.
async fn create(emulator: &Emulator, case: &UpdateCase) {
    let mut request = json!({
        "targets": [thing_arn(case.thing_name)],
        "document": r#"{"operation":"reboot"}"#,
    });
    if let Some(description) = case.created_description {
        request["description"] = json!(description);
    }
    if let Some(minutes) = case.created_timeout_minutes {
        request["timeoutConfig"] = json!({ "inProgressTimeoutInMinutes": minutes });
    }
    let created = emulator.create_job(case.job_id, &request).await;
    assert_eq!(
        created.status, 200,
        "case {:?}: the job must be creatable, got {} {:?}",
        case.case, created.status, created.body
    );
}

/// RULE: a write `UpdateJob` accepts is reflected by every later read of that job — the
/// members it names are stored, the ones it omits are untouched, and `DescribeJob` and
/// `CancelJob` both report the CURRENT record.
#[tokio::test]
async fn an_update_is_reflected_by_every_later_read_of_the_job() {
    let emulator = Emulator::new();

    let cases = vec![
        UpdateCase {
            case: "a description is set on a job created without one",
            job_id: "update-sets-description",
            thing_name: "device-a",
            created_description: None,
            created_timeout_minutes: None,
            update_description: Some("rolling out to the fleet"),
            update_timeout_minutes: None,
            expected_description: Some("rolling out to the fleet"),
            expected_timeout_minutes: None,
        },
        UpdateCase {
            case: "a description is replaced",
            job_id: "update-replaces-description",
            thing_name: "device-b",
            created_description: Some("probe job"),
            created_timeout_minutes: None,
            update_description: Some("updated probe job"),
            update_timeout_minutes: None,
            expected_description: Some("updated probe job"),
            expected_timeout_minutes: None,
        },
        UpdateCase {
            case: "only the timeout moves, so the description is untouched",
            job_id: "update-moves-timeout",
            thing_name: "device-c",
            created_description: Some("install the new firmware"),
            created_timeout_minutes: Some(5),
            update_description: None,
            update_timeout_minutes: Some(Some(120)),
            expected_description: Some("install the new firmware"),
            expected_timeout_minutes: Some(120),
        },
        UpdateCase {
            case: "naming timeoutConfig with no deadline clears the one the job had",
            job_id: "update-clears-timeout",
            thing_name: "device-d",
            created_description: Some("reboot the fleet"),
            created_timeout_minutes: Some(10),
            update_description: None,
            update_timeout_minutes: Some(None),
            expected_description: Some("reboot the fleet"),
            expected_timeout_minutes: None,
        },
        UpdateCase {
            case: "both members move at once",
            job_id: "update-moves-both",
            thing_name: "device-e",
            created_description: None,
            created_timeout_minutes: Some(7),
            update_description: Some("one call moves both"),
            update_timeout_minutes: Some(Some(10_080)),
            expected_description: Some("one call moves both"),
            expected_timeout_minutes: Some(10_080),
        },
        UpdateCase {
            case: "an update naming neither member changes nothing",
            job_id: "update-names-nothing",
            thing_name: "device-f",
            created_description: Some("left as created"),
            created_timeout_minutes: Some(3),
            update_description: None,
            update_timeout_minutes: None,
            expected_description: Some("left as created"),
            expected_timeout_minutes: Some(3),
        },
    ];

    for case in &cases {
        create(&emulator, case).await;

        let mut request = json!({});
        if let Some(description) = case.update_description {
            request["description"] = json!(description);
        }
        if let Some(minutes) = case.update_timeout_minutes {
            request["timeoutConfig"] = match minutes {
                Some(minutes) => json!({ "inProgressTimeoutInMinutes": minutes }),
                None => json!({}),
            };
        }
        let updated = emulator.update_job(case.job_id, &request).await;
        assert_eq!(
            updated.status, 200,
            "case {:?}: expected 200 from UpdateJob, got {} {:?}",
            case.case, updated.status, updated.body
        );
        assert_eq!(
            updated.body,
            Value::Null,
            "case {:?}: UpdateJob is modelled with no output members, got {:?}",
            case.case,
            updated.body
        );

        // The following READ reports the current record, member for member.
        let read = emulator.describe_job(case.job_id).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: expected 200 from DescribeJob, got {} {:?}",
            case.case, read.status, read.body
        );
        assert_eq!(
            read.text("/job/description").as_deref(),
            case.expected_description,
            "case {:?}: expected the current description, got {:?}",
            case.case,
            read.job()
        );
        assert_eq!(
            read.body
                .pointer("/job/timeoutConfig/inProgressTimeoutInMinutes")
                .and_then(Value::as_i64),
            case.expected_timeout_minutes,
            "case {:?}: expected the current timeoutConfig, got {:?}",
            case.case,
            read.job()
        );
        // An update moves no member it does not name: the job is still rolling out under
        // the identity the create minted.
        assert_eq!(
            (
                read.text("/job/jobId"),
                read.text("/job/jobArn"),
                read.text("/job/status")
            ),
            (
                Some(case.job_id.to_string()),
                Some(job_arn(case.job_id)),
                Some(JOB_IN_PROGRESS.to_string())
            ),
            "case {:?}: expected the created identity and a job still rolling out, \
             got {:?}",
            case.case,
            read.job()
        );

        // The timer a device will arm is the job's CURRENT one: the execution nobody has
        // started yet carries whatever the job now states.
        let stored = emulator.stored_execution_timeout(case.thing_name, case.job_id);
        assert_eq!(
            stored, case.expected_timeout_minutes,
            "case {:?}: expected the unstarted execution to carry the job's current \
             in-progress timeout {:?}, got {stored:?}",
            case.case, case.expected_timeout_minutes
        );

        // The cancel's own echo is read from the same record, so it reports the CURRENT
        // description and not the one the job was created with.
        let canceled = emulator.cancel_job(case.job_id).await;
        assert_eq!(
            canceled.status, 200,
            "case {:?}: expected 200 from CancelJob, got {} {:?}",
            case.case, canceled.status, canceled.body
        );
        assert_eq!(
            (
                canceled.text("/jobId"),
                canceled.text("/jobArn"),
                canceled.text("/description").as_deref()
            ),
            (
                Some(case.job_id.to_string()),
                Some(job_arn(case.job_id)),
                case.expected_description
            ),
            "case {:?}: expected the stored identity and the current description, \
             got {:?}",
            case.case,
            canceled.body
        );
    }
}

/// RULE: a `jobId` no job was created under is the modelled `ResourceNotFoundException`
/// (HTTP 404) — never a 200 updating a job that does not exist, and never a 501 — and the
/// refusal writes nothing.
#[tokio::test]
async fn an_unknown_job_id_is_a_404() {
    let emulator = Emulator::new();

    let case = UpdateCase {
        case: "a job that exists",
        job_id: "job-that-exists",
        thing_name: "device-g",
        created_description: Some("as created"),
        created_timeout_minutes: Some(2),
        update_description: None,
        update_timeout_minutes: None,
        expected_description: Some("as created"),
        expected_timeout_minutes: Some(2),
    };
    create(&emulator, &case).await;
    let before = emulator.describe_job(case.job_id).await;

    // An id nobody used, one differing only in case, one differing in a separator, and
    // one that is a prefix of the created id: none of them names a job.
    let unknown = [
        "job-that-exists-not",
        "JOB-THAT-EXISTS",
        "job_that_exists",
        "job",
    ];
    for job_id in unknown {
        let refused = emulator
            .update_job(job_id, &json!({ "description": "no such job" }))
            .await;
        assert_eq!(
            (refused.status, refused.error_code.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {job_id:?}: expected a 404 ResourceNotFoundException, got {} {:?}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.message(),
            format!("Job {job_id} cannot be found."),
            "case {job_id:?}: expected the refusal to name the job, got {:?}",
            refused.body
        );
    }

    // The job that DOES exist is untouched: a refused update of another id wrote nothing.
    let after = emulator.describe_job(case.job_id).await;
    assert_eq!(
        (after.status, after.body.clone()),
        (before.status, before.body.clone()),
        "case {:?}: a refused update of another id must change nothing, got {:?}",
        case.case,
        after.body
    );
}

/// RULE: the same rule over the other target form a job accepts — a `CONTINUOUS` job
/// pointed at a thing GROUP is a job that exists, so an update of its description is
/// stored and the cancel that stops it answers 200 with that job's identifiers and its
/// CURRENT description, never a not-found.
#[tokio::test]
async fn a_group_targeted_continuous_job_is_updated_then_cancelled() {
    let emulator = Emulator::new();

    assert_eq!(
        emulator.create_thing_group("fleet").await.status,
        200,
        "the thing group must be creatable"
    );
    assert_eq!(
        emulator.create_thing("sensor-a").await.status,
        200,
        "the thing must be creatable"
    );
    assert_eq!(
        emulator
            .add_thing_to_thing_group("fleet", "sensor-a")
            .await
            .status,
        200,
        "the membership must be writable"
    );

    // Both target forms of one rule: the group the devices are in, and a device named
    // directly.
    let rows: [(&str, &str, String); 2] = [
        ("a thing-group target", "group-job", thing_group_arn("fleet")),
        ("a thing target", "thing-job", thing_arn("sensor-a")),
    ];
    for (case, job_id, target) in rows {
        let created = emulator
            .create_job(
                job_id,
                &json!({
                    "targets": [target],
                    "document": r#"{"operation":"reboot"}"#,
                    "targetSelection": "CONTINUOUS",
                    "description": "probe job",
                }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the job must be creatable, got {} {:?}",
            created.status, created.body
        );

        let updated = emulator
            .update_job(job_id, &json!({ "description": "updated probe job" }))
            .await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected 200 from UpdateJob, got {} {:?}",
            updated.status, updated.body
        );

        let canceled = emulator.cancel_job(job_id).await;
        assert_eq!(
            canceled.status, 200,
            "case {case:?}: a job that exists and is rolling out must be cancellable, \
             got {} {:?}",
            canceled.status,
            canceled.body
        );
        assert_eq!(
            (
                canceled.text("/jobId"),
                canceled.text("/jobArn"),
                canceled.text("/description")
            ),
            (
                Some(job_id.to_string()),
                Some(job_arn(job_id)),
                Some("updated probe job".to_string())
            ),
            "case {case:?}: expected the job's identifiers and its current description, \
             got {:?}",
            canceled.body
        );

        // The cancel it reported is the state a later reader finds.
        let read = emulator.describe_job(job_id).await;
        assert_eq!(
            (read.status, read.text("/job/status")),
            (200, Some(JOB_CANCELED.to_string())),
            "case {case:?}: expected a {JOB_CANCELED} job, got {} {:?}",
            read.status,
            read.job()
        );
        assert_eq!(
            read.text("/job/description"),
            Some("updated probe job".to_string()),
            "case {case:?}: expected the updated description on the cancelled job, \
             got {:?}",
            read.job()
        );
    }
}
