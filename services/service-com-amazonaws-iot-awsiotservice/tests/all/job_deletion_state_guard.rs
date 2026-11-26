// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the state guard `DeleteJob` applies: WHICH job may be
//! deleted without `force`, and what the refusal of one that may not states.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 `DELETE /jobs/{jobId}` signed for the `iot` signing name — and reads the
//! job back through `DescribeJob` and the shared job-execution store the IoT Jobs data
//! plane serves, so what is asserted is what an SDK client and a device would see.
//!
//! The rule under test: `force` is read against the STATUS THE JOB IS REPORTED IN —
//! "you can only delete a job which is in a terminal state ("COMPLETED" or "CANCELED")
//! or an exception will occur" (`DeleteJob$force`) — so a job in any other status is
//! refused `InvalidStateTransitionException` (409) stating that status, and a job in a
//! terminal one is deleted whatever its executions are still doing. A refused delete
//! writes nothing.
//!
//! The table spans what the rule generalises over: the status the job is reported in
//! (reached by its own lifecycle and by what its targets made of it), how many devices
//! it targets, whether any execution is still pending, and whether the job has an
//! execution at all.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use service_common::iot_job_executions::{
    JobExecutionStore, STATUS_IN_PROGRESS, STATUS_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region this suite signs for.
const REGION: &str = "us-east-1";

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn job_status(&self) -> Option<String> {
        self.body
            .pointer("/job/status")
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it. The
/// engine handle is kept so a case can put a device's execution in a status only the
/// device-facing plane reaches.
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
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host;x-amz-date, Signature=0000"
                ),
            )
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

    async fn create_thing_group(&self, thing_group_name: &str) -> Reply {
        self.call(
            "POST",
            &format!("/thing-groups/{thing_group_name}"),
            Some(&json!({})),
        )
        .await
    }

    async fn create_job(&self, job_id: &str, request: &Value) -> Reply {
        self.call("PUT", &format!("/jobs/{job_id}"), Some(request))
            .await
    }

    async fn cancel_job(&self, job_id: &str) -> Reply {
        self.call("PUT", &format!("/jobs/{job_id}/cancel"), Some(&json!({})))
            .await
    }

    async fn describe_job(&self, job_id: &str) -> Reply {
        self.call("GET", &format!("/jobs/{job_id}"), None).await
    }

    async fn delete_job(&self, job_id: &str, force: bool) -> Reply {
        let query = if force { "?force=true" } else { "" };
        self.call("DELETE", &format!("/jobs/{job_id}{query}"), None)
            .await
    }

    /// Put the stored execution of (thing, job) in `status`, which is how a case reaches
    /// a state only a device reporting on its work produces.
    fn set_execution_status(&self, thing_name: &str, job_id: &str, status: &str) {
        let store = self.engine.state::<JobExecutionStore>();
        let mut store = store.lock();
        let index = store
            .execution_index(thing_name, job_id, None)
            .unwrap_or_else(|| panic!("{thing_name} must hold an execution of {job_id}"));
        store
            .record_mut(thing_name, index)
            .expect("the index came from this store")
            .status = status.to_string();
    }

    /// How many executions of `job_id` the shared store holds for `thing_name`.
    fn executions_of(&self, thing_name: &str, job_id: &str) -> usize {
        let store = self.engine.state::<JobExecutionStore>();
        let store = store.lock();
        store
            .executions_for(thing_name)
            .iter()
            .filter(|record| record.job_id == job_id)
            .count()
    }
}

/// The ARN a `targets` entry names a device by.
fn thing_arn(thing_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing_name}")
}

/// The ARN a `targets` entry names a thing GROUP by.
fn thing_group_arn(thing_group_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/{thing_group_name}")
}

/// What a row does to the job between its create and the delete.
enum Reach {
    /// Nothing: the job is as its create left it.
    AsCreated,
    /// Every targeted device finished its execution with that status.
    EveryDeviceReports(&'static str),
    /// One device started its execution, and then the job was cancelled — so the job's
    /// own status is terminal while an execution of it is still pending.
    StartedThenCancelled,
}

/// One row of the table.
struct DeleteCase {
    case: &'static str,
    job_id: &'static str,
    /// The devices the job targets directly.
    things: &'static [&'static str],
    /// A thing group the job targets as well, when the row names one — a group with no
    /// member is a target that resolves to no device at all.
    group: Option<&'static str>,
    /// `targetSelection`, when the row names one.
    target_selection: Option<&'static str>,
    /// How the job gets from its create to the state the delete meets.
    reach: Reach,
    /// The `JobStatus` the job is reported in when the delete arrives.
    reported_status: &'static str,
    /// Whether that status admits an unforced delete.
    deletable_unforced: bool,
}

/// RULE: `DeleteJob` without `force` is served exactly while the job is reported in a
/// terminal status, and otherwise refused `InvalidStateTransitionException` (409) in the
/// jobs plane's sentence for it — which names the job and the status it was refused in.
#[tokio::test]
async fn an_unforced_delete_is_refused_in_the_status_the_job_is_reported_in() {
    let emulator = Emulator::new();

    let cases: Vec<DeleteCase> = vec![
        DeleteCase {
            case: "a fresh job with one queued device",
            job_id: "delete-queued",
            things: &["guard-device-a"],
            group: None,
            target_selection: None,
            reach: Reach::AsCreated,
            reported_status: "IN_PROGRESS",
            deletable_unforced: false,
        },
        DeleteCase {
            case: "a job one of whose two devices has started its execution",
            job_id: "delete-started",
            things: &["guard-device-b", "guard-device-c"],
            group: None,
            target_selection: Some("SNAPSHOT"),
            reach: Reach::EveryDeviceReports(STATUS_IN_PROGRESS),
            reported_status: "IN_PROGRESS",
            deletable_unforced: false,
        },
        DeleteCase {
            case: "a job whose only target is a group holding no device, so it has no \
                   execution at all",
            job_id: "delete-no-executions",
            things: &[],
            group: Some("guard-empty-fleet"),
            target_selection: Some("CONTINUOUS"),
            reach: Reach::AsCreated,
            reported_status: "IN_PROGRESS",
            deletable_unforced: false,
        },
        DeleteCase {
            case: "a snapshot job every target completed",
            job_id: "delete-completed",
            things: &["guard-device-d", "guard-device-e"],
            group: None,
            target_selection: Some("SNAPSHOT"),
            reach: Reach::EveryDeviceReports(STATUS_SUCCEEDED),
            reported_status: "COMPLETED",
            deletable_unforced: true,
        },
        DeleteCase {
            case: "a snapshot job whose create named no selection, every target completed",
            job_id: "delete-completed-unselected",
            things: &["guard-device-f"],
            group: None,
            target_selection: None,
            reach: Reach::EveryDeviceReports(STATUS_SUCCEEDED),
            reported_status: "COMPLETED",
            deletable_unforced: true,
        },
        DeleteCase {
            case: "a cancelled job whose device is still running its execution",
            job_id: "delete-cancelled",
            things: &["guard-device-g"],
            group: None,
            target_selection: Some("CONTINUOUS"),
            reach: Reach::StartedThenCancelled,
            reported_status: "CANCELED",
            deletable_unforced: true,
        },
    ];

    for case in &cases {
        // The setup: the devices, the group and the job, then the state the row reaches.
        for thing_name in case.things {
            assert_eq!(
                emulator.create_thing(thing_name).await.status,
                200,
                "case {:?}: the targeted device {thing_name} must be registrable",
                case.case
            );
        }
        let mut targets: Vec<String> = case.things.iter().map(|name| thing_arn(name)).collect();
        if let Some(thing_group_name) = case.group {
            assert_eq!(
                emulator.create_thing_group(thing_group_name).await.status,
                200,
                "case {:?}: the targeted group must be creatable",
                case.case
            );
            targets.push(thing_group_arn(thing_group_name));
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
            "case {:?}: the job must be creatable, got {} {:?}",
            case.case, created.status, created.body
        );

        match case.reach {
            Reach::AsCreated => {}
            Reach::EveryDeviceReports(status) => {
                for thing_name in case.things {
                    emulator.set_execution_status(thing_name, case.job_id, status);
                }
            }
            Reach::StartedThenCancelled => {
                for thing_name in case.things {
                    emulator.set_execution_status(thing_name, case.job_id, STATUS_IN_PROGRESS);
                }
                assert_eq!(
                    emulator.cancel_job(case.job_id).await.status,
                    200,
                    "case {:?}: the cancel must succeed",
                    case.case
                );
            }
        }

        // The status the job is reported in is the one the guard reads.
        let read = emulator.describe_job(case.job_id).await;
        assert_eq!(
            read.job_status().as_deref(),
            Some(case.reported_status),
            "case {:?}: expected the job reported in {}, got {:?}",
            case.case,
            case.reported_status,
            read.body
        );

        let unforced = emulator.delete_job(case.job_id, false).await;
        if case.deletable_unforced {
            assert_eq!(
                unforced.status, 200,
                "case {:?}: a job in {} is deletable without force, got {} {:?}",
                case.case, case.reported_status, unforced.status, unforced.body
            );
        } else {
            assert_eq!(
                (unforced.status, unforced.error_code.as_deref()),
                (409, Some("InvalidStateTransitionException")),
                "case {:?}: expected a 409 InvalidStateTransitionException, got {} {:?}",
                case.case,
                unforced.status,
                unforced.body
            );
            assert_eq!(
                unforced.message(),
                format!(
                    "Job {} in status {} cannot be deleted without force flag.",
                    case.job_id, case.reported_status
                ),
                "case {:?}: expected the refusal to name the job and the status it was \
                 refused in, got {:?}",
                case.case,
                unforced.body
            );
            // The refusal wrote nothing: the job and every execution are still there.
            let after = emulator.describe_job(case.job_id).await;
            assert_eq!(
                (after.status, after.job_status().as_deref()),
                (200, Some(case.reported_status)),
                "case {:?}: a refused delete must leave the job as it was, got {} {:?}",
                case.case,
                after.status,
                after.body
            );
            for thing_name in case.things {
                assert_eq!(
                    emulator.executions_of(thing_name, case.job_id),
                    1,
                    "case {:?}: a refused delete must keep {thing_name}'s execution",
                    case.case
                );
            }
            // `force` is what deletes such a job: "when true, you can delete a job which
            // is IN_PROGRESS".
            let forced = emulator.delete_job(case.job_id, true).await;
            assert_eq!(
                forced.status, 200,
                "case {:?}: the forced delete must succeed, got {} {:?}",
                case.case, forced.status, forced.body
            );
        }

        // Whichever way it went, the job and its executions are gone afterwards.
        let gone = emulator.describe_job(case.job_id).await;
        assert_eq!(
            (gone.status, gone.error_code.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {:?}: the deleted job must be gone, got {} {:?}",
            case.case,
            gone.status,
            gone.body
        );
        for thing_name in case.things {
            assert_eq!(
                emulator.executions_of(thing_name, case.job_id),
                0,
                "case {:?}: the deleted job must leave {thing_name} nothing, got {}",
                case.case,
                emulator.executions_of(thing_name, case.job_id)
            );
        }
    }
}

/// RULE: the guard is read from the job's status alone, so a `jobId` the account holds
/// no job under is the modelled `ResourceNotFoundException` (404) — forced or not.
#[tokio::test]
async fn a_job_id_naming_no_job_is_not_found_whether_the_delete_forces_it() {
    let emulator = Emulator::new();
    assert_eq!(
        emulator.create_thing("guard-device-live").await.status,
        200,
        "the targeted device must be registrable"
    );
    assert_eq!(
        emulator
            .create_job(
                "guard-live-job",
                &json!({
                    "targets": [thing_arn("guard-device-live")],
                    "document": "{}",
                }),
            )
            .await
            .status,
        200,
        "the live job must be creatable"
    );

    // An id nobody used, one differing only in case, one differing in a separator, and
    // one that is a prefix of the live id: none of them names a job.
    for job_id in [
        "guard-live-job-not",
        "GUARD-LIVE-JOB",
        "guard_live_job",
        "guard",
    ] {
        for force in [false, true] {
            let reply = emulator.delete_job(job_id, force).await;
            assert_eq!(
                (reply.status, reply.error_code.as_deref()),
                (404, Some("ResourceNotFoundException")),
                "case {job_id:?} (force={force}): expected a 404 ResourceNotFoundException, \
                 got {} {:?}",
                reply.status,
                reply.body
            );
            assert_eq!(
                reply.message(),
                format!("Job {job_id} cannot be found."),
                "case {job_id:?} (force={force}): expected the refusal to name the job, \
                 got {:?}",
                reply.body
            );
        }
    }

    // The live job is untouched by every refused delete of another id.
    let read = emulator.describe_job("guard-live-job").await;
    assert_eq!(
        (read.status, read.job_status().as_deref()),
        (200, Some("IN_PROGRESS")),
        "the live job must be left exactly as it was, got {} {:?}",
        read.status,
        read.body
    );
}
