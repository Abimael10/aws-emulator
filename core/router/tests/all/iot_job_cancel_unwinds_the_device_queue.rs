// SPDX-License-Identifier: CC0-1.0

//! Cancelling an IoT job across TWO services: the control plane cancels a job
//! (`iot` `CancelJob`) and the devices it targeted then poll the Jobs data plane
//! (`iot-jobs-data`), so the cancel is observed where the work was handed out.
//!
//! This suite needs the WHOLE router: the job and its cancel live in the `iot` crate
//! while the executions a device polls for are served by the `iot-jobs-data` crate,
//! and the point under test is that they are ONE set of records — a single-service
//! test cannot see the hand-off at all.
//!
//! The rule under test: cancelling a job ends exactly the executions its `force` flag
//! reaches — `CancelJob$force` is "If true job executions with status IN_PROGRESS and
//! QUEUED are canceled, otherwise only job executions with status QUEUED are
//! canceled" — so from the device's side an ended execution is in neither pending
//! partition, is refused by the data-plane read as terminal, and is no longer what
//! `StartNextPendingJobExecution` hands out (the next QUEUED job is reachable only
//! once it is terminal), while an execution the cancel did not reach is still handed
//! back unchanged; and the job, now terminal itself, is deletable — without `force`
//! exactly when no execution of it is still pending, which is what unwinds the
//! lifecycle `CreateJob` -> dispatch -> `CancelJob` -> `DeleteJob`.
//!
//! Assembling the router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests are driven — on an explicitly sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// The `JobExecutionStatus` of an execution a cancel ended.
const EXECUTION_CANCELED: &str = "CANCELED";
/// The `JobExecutionStatus` of an execution the device has started.
const EXECUTION_IN_PROGRESS: &str = "IN_PROGRESS";

/// A SigV4 credential scope naming `signing_name` in [`REGION`] — the signal the
/// router picks the owning service by.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One device the cancelled job targets: the thing, and whether it had already taken
/// the execution up (`$next`) when the cancel arrived.
struct Device {
    thing_name: &'static str,
    starts_before_cancel: bool,
}

impl Device {
    /// The status the rule leaves this device's execution in: a cancel reaches a
    /// `QUEUED` execution always and a started one only when forced.
    fn status_after_cancel(&self, force: bool) -> &'static str {
        if self.starts_before_cancel && !force {
            EXECUTION_IN_PROGRESS
        } else {
            EXECUTION_CANCELED
        }
    }
}

/// One row of the cancel table.
struct CancelCase {
    case: &'static str,
    /// The job that is cancelled.
    job_id: &'static str,
    /// A second job, created after it and targeting only the row's FIRST device, when
    /// the row has one: what that device must be handed once the first job's execution
    /// is terminal, and nothing before.
    next_job_id: Option<&'static str>,
    /// `CancelJob$force`.
    force: bool,
    /// What the cancel states about itself, when it states anything.
    reason_code: Option<&'static str>,
    comment: Option<&'static str>,
    /// The devices the cancelled job targets.
    devices: &'static [Device],
}

/// The rows differ in the `force` flag, in how many devices the job targets, in
/// whether each of them had started its execution before the cancel, in whether a
/// further job is queued behind it, and in what the cancel states about itself.
const CANCEL_CASES: &[CancelCase] = &[
    CancelCase {
        case: "a queued execution is ended and the next job becomes reachable",
        job_id: "unwind-queued",
        next_job_id: Some("unwind-queued-next"),
        force: false,
        reason_code: None,
        comment: Some("no longer needed"),
        devices: &[Device {
            thing_name: "unwind-laser-1",
            starts_before_cancel: false,
        }],
    },
    CancelCase {
        case: "an unforced cancel leaves the started execution with its device",
        job_id: "unwind-unforced",
        next_job_id: Some("unwind-unforced-next"),
        force: false,
        reason_code: Some("FLEET_RECALL"),
        comment: None,
        devices: &[Device {
            thing_name: "unwind-laser-2",
            starts_before_cancel: true,
        }],
    },
    CancelCase {
        case: "a forced cancel ends the started execution and frees the next job",
        job_id: "unwind-forced",
        next_job_id: Some("unwind-forced-next"),
        force: true,
        reason_code: Some("SUPERSEDED_2"),
        comment: Some("the rollout was recalled"),
        devices: &[Device {
            thing_name: "unwind-laser-3",
            starts_before_cancel: true,
        }],
    },
    CancelCase {
        case: "two devices, one started and one queued, forced",
        job_id: "unwind-fanout-forced",
        next_job_id: None,
        force: true,
        reason_code: None,
        comment: None,
        devices: &[
            Device {
                thing_name: "unwind-laser-4",
                starts_before_cancel: true,
            },
            Device {
                thing_name: "unwind-laser-5",
                starts_before_cancel: false,
            },
        ],
    },
    CancelCase {
        case: "three devices, two started and one queued, unforced",
        job_id: "unwind-fanout-unforced",
        next_job_id: None,
        force: false,
        reason_code: Some("PARTIAL"),
        comment: Some("stop whatever has not started"),
        devices: &[
            Device {
                thing_name: "unwind-laser-6",
                starts_before_cancel: true,
            },
            Device {
                thing_name: "unwind-laser-7",
                starts_before_cancel: true,
            },
            Device {
                thing_name: "unwind-laser-8",
                starts_before_cancel: false,
            },
        ],
    },
];

/// The `jobId`s of one pending partition, in the order the service returned them.
fn job_ids(list: &Value) -> Vec<String> {
    list.as_array()
        .map(|summaries| {
            summaries
                .iter()
                .map(|summary| {
                    summary
                        .get("jobId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn cancelling_a_job_ends_the_executions_its_force_flag_reaches_for_every_device() {
    std::thread::Builder::new()
        .name("iot-job-cancel-unwind".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);

            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            let call = |method: &str, url: String, signing_name: &str, body: String| {
                let request = http::Request::builder()
                    .method(method)
                    .uri(url)
                    .header("authorization", authorization(signing_name))
                    .header("content-type", "application/json")
                    .body(bytes::Bytes::from(body))
                    .expect("a well-formed request");
                let response = runtime.block_on(router.dispatch(request));
                let status = response.status().as_u16();
                let error_code = response
                    .headers()
                    .get("x-amzn-errortype")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let rendered = String::from_utf8_lossy(response.body()).to_string();
                let parsed: Value = serde_json::from_str(&rendered).unwrap_or(Value::Null);
                (status, error_code, rendered, parsed)
            };

            // The control plane, signed for `iot`.
            let control = |method: &str, path: String, body: String| {
                call(
                    method,
                    format!("https://iot.{REGION}.amazonaws.com{path}"),
                    "iot",
                    body,
                )
            };
            // The Jobs data plane, signed for `iot-jobs-data`.
            let device = |method: &str, path: String, body: String| {
                call(
                    method,
                    format!("https://data.jobs.iot.{REGION}.amazonaws.com{path}"),
                    "iot-jobs-data",
                    body,
                )
            };

            for case in CANCEL_CASES {
                let label = case.case;

                for target in case.devices {
                    let thing = target.thing_name;
                    let (status, _, rendered, _) =
                        control("POST", format!("/things/{thing}"), "{}".to_string());
                    assert_eq!(
                        status, 200,
                        "case {label:?}: registering {thing} must answer 200, got {status} \
                         with {rendered}"
                    );
                }

                // The job the cancel will unwind, targeting every device of this row.
                let request = json!({
                    "targets": case
                        .devices
                        .iter()
                        .map(|target| format!(
                            "arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{}",
                            target.thing_name
                        ))
                        .collect::<Vec<String>>(),
                    "description": "the rollout under test",
                    "document": r#"{"operation":"reboot"}"#,
                });
                let (status, _, rendered, _) =
                    control("PUT", format!("/jobs/{}", case.job_id), request.to_string());
                assert_eq!(
                    status, 200,
                    "case {label:?}: creating {} must answer 200, got {status} with {rendered}",
                    case.job_id
                );

                // A second job queued BEHIND it on the row's first device, where the row
                // has one.
                let first_thing = case.devices[0].thing_name;
                if let Some(next_job_id) = case.next_job_id {
                    let request = json!({
                        "targets": [format!(
                            "arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{first_thing}"
                        )],
                        "document": r#"{"operation":"install"}"#,
                    });
                    let (status, _, rendered, _) =
                        control("PUT", format!("/jobs/{next_job_id}"), request.to_string());
                    assert_eq!(
                        status, 200,
                        "case {label:?}: creating {next_job_id} must answer 200, got {status} \
                         with {rendered}"
                    );
                }

                // Every targeted device holds the job queued for it, and the row's first
                // device holds the job behind it queued after it: a device is handed one
                // execution at a time, in the order they were queued.
                for target in case.devices {
                    let thing = target.thing_name;
                    let (status, _, rendered, parsed) =
                        device("GET", format!("/things/{thing}/jobs"), String::new());
                    assert_eq!(
                        status, 200,
                        "case {label:?}: {thing} must be able to poll, got {status} \
                         with {rendered}"
                    );
                    let mut expected = vec![case.job_id.to_string()];
                    if thing == first_thing {
                        if let Some(next_job_id) = case.next_job_id {
                            expected.push(next_job_id.to_string());
                        }
                    }
                    assert_eq!(
                        job_ids(&parsed["queuedJobs"]),
                        expected,
                        "case {label:?}: expected {expected:?} queued for {thing}, \
                         got {rendered}"
                    );
                }

                // Every device that takes its work up before the cancel does so, and is
                // handed THIS job — the one queued first — not the one behind it.
                let mut handed_out: Vec<Option<(i64, i64)>> = Vec::new();
                for target in case.devices {
                    if !target.starts_before_cancel {
                        handed_out.push(None);
                        continue;
                    }
                    let thing = target.thing_name;
                    let (status, _, rendered, parsed) = device(
                        "PUT",
                        format!("/things/{thing}/jobs/$next"),
                        "{}".to_string(),
                    );
                    assert_eq!(
                        status, 200,
                        "case {label:?}: $next for {thing} must answer 200, got {status} \
                         with {rendered}"
                    );
                    assert_eq!(
                        (
                            parsed["execution"]["jobId"].as_str(),
                            parsed["execution"]["status"].as_str()
                        ),
                        (Some(case.job_id), Some(EXECUTION_IN_PROGRESS)),
                        "case {label:?}: {thing} must be handed {} in progress, got {rendered}",
                        case.job_id
                    );
                    let version = parsed["execution"]["versionNumber"]
                        .as_i64()
                        .unwrap_or_default();
                    let started_at = parsed["execution"]["startedAt"]
                        .as_i64()
                        .unwrap_or_default();
                    assert!(
                        version > 0 && started_at > 0,
                        "case {label:?}: a started execution carries a version and a start \
                         time, got {rendered}"
                    );
                    handed_out.push(Some((version, started_at)));
                }

                // THE CANCEL.
                let mut body = json!({});
                if let Some(reason_code) = case.reason_code {
                    body["reasonCode"] = json!(reason_code);
                }
                if let Some(comment) = case.comment {
                    body["comment"] = json!(comment);
                }
                let query = if case.force { "?force=true" } else { "" };
                let (status, _, rendered, parsed) = control(
                    "PUT",
                    format!("/jobs/{}/cancel{query}", case.job_id),
                    body.to_string(),
                );
                assert_eq!(
                    status, 200,
                    "case {label:?}: the cancel must answer 200, got {status} with {rendered}"
                );
                let expected_arn =
                    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:job/{}", case.job_id);
                assert_eq!(
                    (
                        parsed["jobId"].as_str(),
                        parsed["jobArn"].as_str(),
                        parsed["description"].as_str()
                    ),
                    (
                        Some(case.job_id),
                        Some(expected_arn.as_str()),
                        Some("the rollout under test")
                    ),
                    "case {label:?}: the cancel must answer the stored job identity, \
                     got {rendered}"
                );

                // The control plane reports the job itself as cancelled, with what the
                // cancel stated about it.
                let (status, _, rendered, parsed) =
                    control("GET", format!("/jobs/{}", case.job_id), String::new());
                assert_eq!(
                    (status, parsed["job"]["status"].as_str()),
                    (200, Some("CANCELED")),
                    "case {label:?}: expected a CANCELED job, got {status} with {rendered}"
                );
                assert_eq!(
                    (
                        parsed["job"]["forceCanceled"].as_bool(),
                        parsed["job"]["reasonCode"].as_str(),
                        parsed["job"]["comment"].as_str()
                    ),
                    (Some(case.force), case.reason_code, case.comment),
                    "case {label:?}: expected the stated cancel recorded on the job, \
                     got {rendered}"
                );

                // Each device now sees exactly what the rule leaves it: an ended
                // execution is in neither partition and is refused as terminal, while one
                // the cancel did not reach is still in progress for it.
                for (target, before) in case.devices.iter().zip(&handed_out) {
                    let thing = target.thing_name;
                    let expected = target.status_after_cancel(case.force);
                    let ended = expected == EXECUTION_CANCELED;

                    let (status, _, rendered, parsed) =
                        device("GET", format!("/things/{thing}/jobs"), String::new());
                    assert_eq!(
                        status, 200,
                        "case {label:?}: {thing} must be able to poll, got {status} \
                         with {rendered}"
                    );
                    assert!(
                        !job_ids(&parsed["queuedJobs"]).contains(&case.job_id.to_string()),
                        "case {label:?}: a cancelled job is queued for nobody, got {rendered}"
                    );
                    assert_eq!(
                        job_ids(&parsed["inProgressJobs"]).contains(&case.job_id.to_string()),
                        !ended,
                        "case {label:?}: expected {} in progress for {thing} to be {}, \
                         got {rendered}",
                        case.job_id,
                        !ended,
                    );

                    let (status, error_code, rendered, parsed) = device(
                        "GET",
                        format!("/things/{thing}/jobs/{}", case.job_id),
                        String::new(),
                    );
                    if ended {
                        assert_eq!(
                            (status, error_code.as_str()),
                            (410, "TerminalStateException"),
                            "case {label:?}: an ended execution is refused as terminal for \
                             {thing}, got {status} {error_code} with {rendered}"
                        );
                    } else {
                        assert_eq!(
                            (status, parsed["execution"]["status"].as_str()),
                            (200, Some(expected)),
                            "case {label:?}: expected {expected} for {thing}, got {status} \
                             with {rendered}"
                        );
                        let (version, started_at) = before.expect(
                            "an execution the cancel did not reach is one the device started",
                        );
                        assert_eq!(
                            (
                                parsed["execution"]["versionNumber"].as_i64(),
                                parsed["execution"]["startedAt"].as_i64()
                            ),
                            (Some(version), Some(started_at)),
                            "case {label:?}: an execution the cancel did not reach keeps the \
                             version and start time {thing} was handed, got {rendered}"
                        );
                    }
                }

                // What the row's FIRST device is handed NEXT: the job queued behind this
                // one once this one's execution is terminal, the same execution again
                // while it is not, and nothing at all when no further job was created.
                let (status, _, rendered, parsed) = device(
                    "PUT",
                    format!("/things/{first_thing}/jobs/$next"),
                    "{}".to_string(),
                );
                assert_eq!(
                    status, 200,
                    "case {label:?}: $next after the cancel must answer 200, got {status} \
                     with {rendered}"
                );
                let survived = case.devices[0].status_after_cancel(case.force)
                    == EXECUTION_IN_PROGRESS;
                let expected_next = if survived {
                    Some(case.job_id)
                } else {
                    case.next_job_id
                };
                assert_eq!(
                    parsed["execution"]["jobId"].as_str(),
                    expected_next,
                    "case {label:?}: expected {expected_next:?} handed to {first_thing} after \
                     the cancel, got {rendered}"
                );
                if survived {
                    let (version, started_at) = handed_out[0]
                        .expect("a surviving execution is one the device started");
                    // A poll that carries neither member and resolves to an execution
                    // already in progress re-hands it unchanged.
                    assert_eq!(
                        (
                            parsed["execution"]["versionNumber"].as_i64(),
                            parsed["execution"]["startedAt"].as_i64()
                        ),
                        (Some(version), Some(started_at)),
                        "case {label:?}: re-handing an in-progress execution must not move \
                         its version or start time, got {rendered}"
                    );
                }

                // And the unwind completes: `force` on `DeleteJob` is read against the
                // JOB's own status, and a cancelled job has reached a status it never
                // leaves — so the unforced delete is served whatever its executions are
                // still doing, and afterwards neither plane holds the job or them.
                let (status, error_code, rendered, _) = control(
                    "DELETE",
                    format!("/jobs/{}", case.job_id),
                    String::new(),
                );
                assert_eq!(
                    status, 200,
                    "case {label:?}: deleting a cancelled job needs no force, got {status} \
                     {error_code} with {rendered}"
                );

                let (status, error_code, rendered, _) =
                    control("GET", format!("/jobs/{}", case.job_id), String::new());
                assert_eq!(
                    (status, error_code.as_str()),
                    (404, "ResourceNotFoundException"),
                    "case {label:?}: a deleted job is not found, got {status} {error_code} \
                     with {rendered}"
                );
                for target in case.devices {
                    let thing = target.thing_name;
                    let (status, error_code, rendered, _) = device(
                        "GET",
                        format!("/things/{thing}/jobs/{}", case.job_id),
                        String::new(),
                    );
                    assert_eq!(
                        (status, error_code.as_str()),
                        (404, "ResourceNotFoundException"),
                        "case {label:?}: a deleted job's execution is gone from {thing}, \
                         got {status} {error_code} with {rendered}"
                    );
                }
            }
        })
        .expect("spawn the job-cancel unwind thread")
        .join()
        .expect("cancelling a job must unwind the executions it handed its devices");
}
