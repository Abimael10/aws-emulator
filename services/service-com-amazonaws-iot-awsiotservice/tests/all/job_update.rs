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
//!   that refusal writes nothing;
//! * the SCOPE of each of those writes: an update moves no member of the record it does
//!   not name — the document and its substitution parameters, the targets, the target
//!   selection, the status, the instant the job was created and the progress counted from
//!   its executions all read back as created, whatever the job's form — a timeout it
//!   names reaches EVERY unstarted execution of THAT job (one per targeted device, a
//!   thing-group target counting once per member) and no execution of any other job, and
//!   successive updates ACCUMULATE on the one record rather than resetting it to the
//!   values the job was created with.
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

/// The members of a reported `Job` an `UpdateJob` call may move: the two the operation
/// writes (`description` and `timeoutConfig`, from
/// `timeoutConfig.inProgressTimeoutInMinutes`) and `lastUpdatedAt`, "the time … when the
/// job was last updated". Every other member is one no update names, which is what
/// [`Reply::without_the_updatable_members`] compares. Should the operation gain another
/// member it writes, it belongs here.
const UPDATABLE_MEMBERS: [&str; 3] = ["description", "timeoutConfig", "lastUpdatedAt"];

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

    /// A numeric member — the epoch-second timestamps a job reports, as restJson1
    /// renders them.
    fn number(&self, pointer: &str) -> Option<f64> {
        self.body.pointer(pointer).and_then(Value::as_f64)
    }

    /// The in-progress timeout the job reports, in minutes.
    fn reported_timeout_minutes(&self) -> Option<i64> {
        self.body
            .pointer("/job/timeoutConfig/inProgressTimeoutInMinutes")
            .and_then(Value::as_i64)
    }

    /// The whole answer with [`UPDATABLE_MEMBERS`] removed, so what is left is everything
    /// an update must NOT have touched.
    fn without_the_updatable_members(&self) -> Value {
        let mut reduced = self.body.clone();
        if let Some(job) = reduced.get_mut("job").and_then(Value::as_object_mut) {
            for member in UPDATABLE_MEMBERS {
                job.remove(member);
            }
        }
        reduced
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

    /// The job document as a reader sees it — `before_substitution` selects the document
    /// as written or the document with the job's parameters resolved.
    async fn job_document(&self, job_id: &str, before_substitution: bool) -> Reply {
        self.call(
            "GET",
            &format!("/jobs/{job_id}/job-document?beforeSubstitution={before_substitution}"),
            None,
        )
        .await
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

/// One row of the partial-update table: a job form, and the description the update
/// writes onto it.
struct JobForm {
    case: &'static str,
    job_id: &'static str,
    /// The devices the job names directly.
    things: &'static [&'static str],
    target_selection: Option<&'static str>,
    document: Option<&'static str>,
    document_source: Option<&'static str>,
    /// `documentParameters`: the values the stored document's placeholders resolve to.
    parameters: &'static [(&'static str, &'static str)],
    created_description: Option<&'static str>,
    created_timeout_minutes: Option<i64>,
    update_description: &'static str,
}

/// RULE: `UpdateJob` is a PARTIAL update — a member the request omits names no change —
/// so an update that names only `description` moves that member and leaves every other
/// member of the record, in every job form, exactly as the job was created.
///
/// The rows span the forms a stored job takes: how many devices it names, whether it
/// carries a target selection, an inline document, substitution parameters or an S3
/// document source, and whether it was created with a description or a timeout at all.
#[tokio::test]
async fn an_update_moves_only_the_members_it_names() {
    let emulator = Emulator::new();

    let forms = [
        JobForm {
            case: "a snapshot job over three devices, with a document and a timeout",
            job_id: "scope-snapshot",
            things: &["scope-a", "scope-b", "scope-c"],
            target_selection: Some("SNAPSHOT"),
            document: Some(r#"{"operation":"install"}"#),
            document_source: None,
            parameters: &[],
            created_description: Some("install the new firmware"),
            created_timeout_minutes: Some(5),
            update_description: "install the new firmware, phase two",
        },
        JobForm {
            case: "a continuous job whose document carries substitution parameters",
            job_id: "scope-parameters",
            things: &["scope-d"],
            target_selection: Some("CONTINUOUS"),
            document: Some(r#"{"operation":"${aws:iot:parameter:action}"}"#),
            document_source: None,
            parameters: &[("action", "reboot"), ("retries", "2")],
            created_description: None,
            created_timeout_minutes: None,
            update_description: "reboot what the parameters name",
        },
        JobForm {
            case: "a job created from a documentSource",
            job_id: "scope-document-source",
            things: &["scope-e", "scope-f"],
            target_selection: None,
            document: None,
            document_source: Some("https://s3.us-east-1.amazonaws.com/updates/payload.json"),
            parameters: &[],
            created_description: Some("roll out the payload"),
            created_timeout_minutes: Some(10_080),
            update_description: "roll out the payload to the rest of the fleet",
        },
    ];

    for form in &forms {
        let targets: Vec<String> = form.things.iter().map(|thing| thing_arn(thing)).collect();
        let mut request = json!({ "targets": targets });
        if let Some(target_selection) = form.target_selection {
            request["targetSelection"] = json!(target_selection);
        }
        if let Some(document) = form.document {
            request["document"] = json!(document);
        }
        if let Some(document_source) = form.document_source {
            request["documentSource"] = json!(document_source);
        }
        if let Some(description) = form.created_description {
            request["description"] = json!(description);
        }
        if let Some(minutes) = form.created_timeout_minutes {
            request["timeoutConfig"] = json!({ "inProgressTimeoutInMinutes": minutes });
        }
        if !form.parameters.is_empty() {
            request["documentParameters"] = Value::Object(
                form.parameters
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), json!(value)))
                    .collect(),
            );
        }
        let created = emulator.create_job(form.job_id, &request).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: the job must be creatable, got {} {:?}",
            form.case, created.status, created.body
        );

        // The record as created, and both views of its document, are what every
        // assertion below is made against.
        let before = emulator.describe_job(form.job_id).await;
        assert_eq!(
            before.status, 200,
            "case {:?}: expected 200 from DescribeJob, got {} {:?}",
            form.case, before.status, before.body
        );
        let created_at = before
            .number("/job/createdAt")
            .unwrap_or_else(|| panic!("case {:?}: a job reports when it was created", form.case));
        assert_eq!(
            before.number("/job/lastUpdatedAt"),
            Some(created_at),
            "case {:?}: nothing has updated the job yet, so it was last written when it \
             was created, got {:?}",
            form.case,
            before.job()
        );
        let document_as_written = emulator.job_document(form.job_id, true).await;
        let document_resolved = emulator.job_document(form.job_id, false).await;

        let updated = emulator
            .update_job(
                form.job_id,
                &json!({ "description": form.update_description }),
            )
            .await;
        assert_eq!(
            updated.status, 200,
            "case {:?}: expected 200 from UpdateJob, got {} {:?}",
            form.case, updated.status, updated.body
        );

        let after = emulator.describe_job(form.job_id).await;
        assert_eq!(
            after.status, 200,
            "case {:?}: expected 200 from DescribeJob, got {} {:?}",
            form.case, after.status, after.body
        );
        // The member the request named is the member that moved.
        assert_eq!(
            after.text("/job/description").as_deref(),
            Some(form.update_description),
            "case {:?}: expected the description the update wrote, got {:?}",
            form.case,
            after.job()
        );
        // The timeout the request did not name is untouched — including on the job that
        // never had one, which still reports none rather than a default.
        assert_eq!(
            after.reported_timeout_minutes(),
            form.created_timeout_minutes,
            "case {:?}: an update naming no timeoutConfig leaves the created one, \
             expected {:?}, got {:?}",
            form.case,
            form.created_timeout_minutes,
            after.job()
        );
        // And so is everything else the record carries: the document source, the
        // targets, the target selection, the parameters, the status and the progress
        // counted from the job's executions.
        assert_eq!(
            after.without_the_updatable_members(),
            before.without_the_updatable_members(),
            "case {:?}: a partial update must move no member it does not name, got {:?}",
            form.case,
            after.body
        );
        // `Job$lastUpdatedAt` is "the time … when the job was last updated": the update
        // is that write, and it never moves when the job was created.
        assert_eq!(
            after.number("/job/createdAt"),
            Some(created_at),
            "case {:?}: an update never moves createdAt, got {:?}",
            form.case,
            after.job()
        );
        let last_updated_at = after.number("/job/lastUpdatedAt").unwrap_or_else(|| {
            panic!(
                "case {:?}: an updated job reports when it was last updated",
                form.case
            )
        });
        assert!(
            last_updated_at >= created_at,
            "case {:?}: expected lastUpdatedAt at or after createdAt {created_at}, \
             got {last_updated_at}",
            form.case
        );

        // The document is not a member `UpdateJob` names, so both views of it are still
        // the ones the create stored.
        for (view, reference) in [(true, &document_as_written), (false, &document_resolved)] {
            let read = emulator.job_document(form.job_id, view).await;
            assert_eq!(
                (read.status, read.body.clone()),
                (reference.status, reference.body.clone()),
                "case {:?}: expected the stored document (beforeSubstitution={view}) to \
                 survive the update, got {} {:?}",
                form.case,
                read.status,
                read.body
            );
        }
    }
}

/// RULE: `timeoutConfig.inProgressTimeoutInMinutes` is "the amount of time each device
/// has to finish its execution of the job", so an update of it reaches every execution of
/// THAT job that no device has started yet — one per targeted device, a thing-group
/// target counting once per member — and reaches no execution of any other job, even on a
/// device the two jobs share.
#[tokio::test]
async fn an_updated_timeout_reaches_every_unstarted_execution_of_that_job_alone() {
    let emulator = Emulator::new();

    // A group with two members, so one target names two devices.
    assert_eq!(
        emulator.create_thing_group("timeout-fleet").await.status,
        200,
        "the thing group must be creatable"
    );
    for member in ["fleet-member-1", "fleet-member-2"] {
        assert_eq!(
            emulator.create_thing(member).await.status,
            200,
            "the thing {member} must be creatable"
        );
        assert_eq!(
            emulator
                .add_thing_to_thing_group("timeout-fleet", member)
                .await
                .status,
            200,
            "the membership of {member} must be writable"
        );
    }

    // The job under update: three devices named directly plus the group's two members,
    // so five executions nobody has started.
    let direct = ["timeout-device-1", "timeout-device-2", "timeout-device-3"];
    let carrier_devices: Vec<String> = direct
        .iter()
        .map(|thing| (*thing).to_string())
        .chain(["fleet-member-1".to_string(), "fleet-member-2".to_string()])
        .collect();
    let mut carrier_targets: Vec<String> = direct.iter().map(|thing| thing_arn(thing)).collect();
    carrier_targets.push(thing_group_arn("timeout-fleet"));
    assert_eq!(
        emulator
            .create_job(
                "timeout-carrier",
                &json!({
                    "targets": carrier_targets,
                    "document": r#"{"operation":"install"}"#,
                    "timeoutConfig": { "inProgressTimeoutInMinutes": 5 },
                }),
            )
            .await
            .status,
        200,
        "the job under update must be creatable"
    );

    // Another job over devices the first one also targets: nothing an update of one job
    // does may reach the other job's executions.
    let bystander_devices = ["timeout-device-1", "fleet-member-2"];
    assert_eq!(
        emulator
            .create_job(
                "timeout-bystander",
                &json!({
                    "targets": bystander_devices
                        .iter()
                        .map(|thing| thing_arn(thing))
                        .collect::<Vec<String>>(),
                    "document": r#"{"operation":"reboot"}"#,
                    "timeoutConfig": { "inProgressTimeoutInMinutes": 60 },
                }),
            )
            .await
            .status,
        200,
        "the other job must be creatable"
    );

    /// One step of the rollout: what the update names, and what every execution of the
    /// updated job must then arm its timer from.
    struct Step {
        case: &'static str,
        /// The update's body, or `Value::Null` for the state as created.
        request: Value,
        carrier_minutes: Option<i64>,
    }

    // The in-progress timeout the other job keeps throughout, whatever the updated job
    // does: its own created value.
    const BYSTANDER_MINUTES: i64 = 60;

    let steps = [
        Step {
            case: "as created, before any update",
            request: Value::Null,
            carrier_minutes: Some(5),
        },
        Step {
            case: "the timeout is raised",
            request: json!({ "timeoutConfig": { "inProgressTimeoutInMinutes": 120 } }),
            carrier_minutes: Some(120),
        },
        Step {
            case: "the timeout is lowered",
            request: json!({ "timeoutConfig": { "inProgressTimeoutInMinutes": 1 } }),
            carrier_minutes: Some(1),
        },
        Step {
            // `timeoutConfig` is a STRUCTURE whose `inProgressTimeoutInMinutes` is
            // optional, and a partial update that names the structure replaces the
            // configuration it stands for — so one carrying no deadline leaves the job
            // stating none, which is the same rule the single-update table above pins.
            case: "naming timeoutConfig with no deadline clears it on every execution",
            request: json!({ "timeoutConfig": {} }),
            carrier_minutes: None,
        },
        Step {
            case: "an update naming only the description re-arms nothing",
            request: json!({ "description": "the timer is unchanged" }),
            carrier_minutes: None,
        },
    ];

    for step in &steps {
        if step.request.is_object() {
            let updated = emulator.update_job("timeout-carrier", &step.request).await;
            assert_eq!(
                updated.status, 200,
                "case {:?}: expected 200 from UpdateJob, got {} {:?}",
                step.case, updated.status, updated.body
            );
        }

        // Every execution of the updated job — one per named device and one per group
        // member — carries the job's current timeout.
        for thing_name in &carrier_devices {
            let stored = emulator.stored_execution_timeout(thing_name, "timeout-carrier");
            assert_eq!(
                stored, step.carrier_minutes,
                "case {:?}: expected {thing_name}'s unstarted execution of \
                 timeout-carrier to carry {:?}, got {stored:?}",
                step.case, step.carrier_minutes
            );
        }
        // The job reports the same value its executions were handed.
        let read = emulator.describe_job("timeout-carrier").await;
        assert_eq!(
            read.reported_timeout_minutes(),
            step.carrier_minutes,
            "case {:?}: expected DescribeJob to report {:?}, got {:?}",
            step.case,
            step.carrier_minutes,
            read.job()
        );

        // The other job is untouched, on the devices it shares with the updated one too.
        for thing_name in bystander_devices {
            let stored = emulator.stored_execution_timeout(thing_name, "timeout-bystander");
            assert_eq!(
                stored,
                Some(BYSTANDER_MINUTES),
                "case {:?}: expected {thing_name}'s execution of timeout-bystander to \
                 keep its own {BYSTANDER_MINUTES}-minute timeout, got {stored:?}",
                step.case
            );
        }
        let bystander = emulator.describe_job("timeout-bystander").await;
        assert_eq!(
            bystander.reported_timeout_minutes(),
            Some(BYSTANDER_MINUTES),
            "case {:?}: expected the other job to keep its own timeout, got {:?}",
            step.case,
            bystander.job()
        );
    }
}

/// RULE: successive updates ACCUMULATE on the one record — each call writes the members it
/// names and leaves whatever an earlier call wrote — so the job is never reset to the
/// values it was created with.
#[tokio::test]
async fn successive_updates_accumulate_on_the_record() {
    let emulator = Emulator::new();

    assert_eq!(
        emulator
            .create_job(
                "update-sequence",
                &json!({
                    "targets": [thing_arn("sequence-device")],
                    "document": r#"{"operation":"install"}"#,
                    "description": "as created",
                    "timeoutConfig": { "inProgressTimeoutInMinutes": 5 },
                }),
            )
            .await
            .status,
        200,
        "the job must be creatable"
    );

    // (case, the update's body, the description every later read must report, the
    // in-progress timeout it must report)
    let steps: [(&str, Value, &str, Option<i64>); 6] = [
        (
            "the description alone moves",
            json!({ "description": "step one" }),
            "step one",
            Some(5),
        ),
        (
            "the timeout alone moves, keeping the description step one wrote",
            json!({ "timeoutConfig": { "inProgressTimeoutInMinutes": 15 } }),
            "step one",
            Some(15),
        ),
        (
            "both members move at once",
            json!({
                "description": "step three",
                "timeoutConfig": { "inProgressTimeoutInMinutes": 45 },
            }),
            "step three",
            Some(45),
        ),
        (
            "an update naming neither member keeps both",
            json!({}),
            "step three",
            Some(45),
        ),
        (
            "the timeout is cleared, keeping the description",
            json!({ "timeoutConfig": {} }),
            "step three",
            None,
        ),
        (
            "the description moves again, leaving the cleared timeout cleared",
            json!({ "description": "step six" }),
            "step six",
            None,
        ),
    ];

    for (case, request, expected_description, expected_minutes) in steps {
        let updated = emulator.update_job("update-sequence", &request).await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected 200 from UpdateJob, got {} {:?}",
            updated.status, updated.body
        );

        let read = emulator.describe_job("update-sequence").await;
        assert_eq!(
            (
                read.text("/job/description").as_deref(),
                read.reported_timeout_minutes()
            ),
            (Some(expected_description), expected_minutes),
            "case {case:?}: expected the accumulated record ({expected_description:?}, \
             {expected_minutes:?}), got {:?}",
            read.job()
        );
        let stored = emulator.stored_execution_timeout("sequence-device", "update-sequence");
        assert_eq!(
            stored, expected_minutes,
            "case {case:?}: expected the unstarted execution to carry the job's current \
             timeout {expected_minutes:?}, got {stored:?}"
        );
    }
}
