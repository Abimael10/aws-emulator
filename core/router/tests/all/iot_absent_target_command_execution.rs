// SPDX-License-Identifier: CC0-1.0

//! The ABSENT-TARGET command-execution flow across TWO services: the device's own plane
//! starts a command execution against a `targetArn` whose thing the IoT registry does not
//! hold (`iot-jobs-data` `StartCommandExecution`), and the control plane then reads it,
//! lists it and removes it (`iot` `GetCommandExecution`, `ListCommandExecutions`,
//! `DeleteCommandExecution`).
//!
//! This suite needs the WHOLE router: the execution is written by one crate and read by
//! another, and the point under test is that the reader addresses the very record the
//! start left — a single-service test cannot see the hand-off.
//!
//! The rule under test: a command-execution `targetArn` names its DEVICE by the ARN alone
//! — its form, service, partition, account and resource type — so the thing registry
//! decides nothing about it. A start the registry does not back is accepted, and from then
//! on every reader of that execution answers from the EXECUTION store: 200 with the record
//! and the `targetArn` echoed verbatim, the `TIMED_OUT` its armed timer owes
//! (`StartCommandExecution$executionTimeoutSeconds`: "A timer is started as soon as the
//! command execution is created. If the command execution status is not set to another
//! terminal state before the timer expires, it will automatically update to TIMED_OUT"),
//! the same record in the listing, the modelled 404 for an `executionId` that device holds
//! nothing under — and reading or deleting it registers no thing, so `ListThings` never
//! reports the device.
//!
//! The rows differ along what the rule generalises over: whether the start arms a timer and
//! how long for (the modelled minimum of 1 second, none at all, and a span no case waits
//! out), the form of the absent device's name (`ThingName`: len 1..128,
//! `^[a-zA-Z0-9:_-]+$`), and whether a device ever reports an outcome for the execution.
//!
//! Assembling the router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests are driven — on an explicitly sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};

// The one percent-encoder this test crate has; a `targetArn` in a query string needs
// exactly that encoding.
use crate::query_wire::form_encode;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// How long a poll for a status the CLOCK owes may keep looking before the suite gives up.
///
/// Generous on purpose: the only tight span in this suite is the timer a row arms, and the
/// poll looks again for the CONDITION rather than waiting a fixed time, so a loaded host
/// takes longer and still passes.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The service's wording for a delete of an execution that has not become deletable,
/// spelled out here rather than read from the crate under test.
const NOT_TERMINAL_MESSAGE: &str =
    "Failed to delete command execution as it hasn't become terminal.";

/// A SigV4 credential scope naming `signing_name` in [`REGION`] — the signal the router
/// picks the owning service by.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One row of the absent-target table.
struct AbsentTargetCase {
    case: &'static str,
    command_id: &'static str,
    /// The device the execution is started against. NO `CreateThing` is ever made for it.
    thing_name: &'static str,
    /// `executionTimeoutSeconds` the start carries, when the row arms a timer
    /// (`CommandExecutionTimeoutInSeconds` is modelled `range 1..`).
    timeout_seconds: Option<i64>,
    /// Whether the row waits the armed timer out and expects `TIMED_OUT`.
    waits_for_the_timer: bool,
    /// Whether a DEVICE reports an outcome for the execution, which is what makes it
    /// deletable.
    device_reports_an_outcome: bool,
}

const ABSENT_TARGET_CASES: &[AbsentTargetCase] = &[
    AbsentTargetCase {
        case: "the shortest timer the model admits, which no device ever picks up",
        command_id: "ghost-reboot",
        thing_name: "ghost-laser-1",
        timeout_seconds: Some(1),
        waits_for_the_timer: true,
        device_reports_an_outcome: false,
    },
    AbsentTargetCase {
        case: "no timer at all, and the device reports an outcome",
        command_id: "ghost_set-temperature",
        thing_name: "ghost:laser_2-A",
        timeout_seconds: None,
        waits_for_the_timer: false,
        device_reports_an_outcome: true,
    },
    AbsentTargetCase {
        case: "a timer no case waits out, so the execution stays CREATED",
        command_id: "ghost-eject",
        thing_name: "GHOST-laser-3",
        timeout_seconds: Some(1_800),
        waits_for_the_timer: false,
        device_reports_an_outcome: false,
    },
];

#[test]
fn a_command_execution_of_an_unregistered_target_is_read_listed_and_deleted() {
    std::thread::Builder::new()
        .name("iot-absent-target-command-execution".to_string())
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
            // The Jobs data plane the device talks to, signed for `iot-jobs-data`.
            let device = |method: &str, path: String, body: String| {
                call(
                    method,
                    format!("https://data.jobs.iot.{REGION}.amazonaws.com{path}"),
                    "iot-jobs-data",
                    body,
                )
            };

            // The thing names `ListThings` reports, which is what the account's REGISTRY
            // holds: no call of this suite may put an absent target in it.
            let registered_things = || -> Vec<String> {
                let (status, _, rendered, listed) =
                    control("GET", "/things".to_string(), String::new());
                assert_eq!(
                    status, 200,
                    "ListThings must answer 200, got {status} with {rendered}"
                );
                listed["things"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|thing| thing["thingName"].as_str().map(str::to_string))
                    .collect()
            };

            for case in ABSENT_TARGET_CASES {
                let label = case.case;
                let thing = case.thing_name;
                let target = format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing}");
                let command =
                    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:command/{}", case.command_id);

                // The command the execution runs has to EXIST: the control plane creates
                // it, and the data plane resolves the `commandArn` against that record.
                let (status, _, rendered, _) = control(
                    "PUT",
                    format!("/commands/{}", case.command_id),
                    "{}".to_string(),
                );
                assert_eq!(
                    status, 200,
                    "case {label:?}: CreateCommand must answer 200, got {status} with \
                     {rendered}"
                );

                // The premise of the row: the DEVICE is in no registry, and stays out of
                // it for the whole case. Nothing here ever calls `CreateThing` for it.
                let registered = registered_things();
                assert!(
                    !registered.iter().any(|held| held == thing),
                    "case {label:?}: the device must be absent from the registry, got \
                     {registered:?}"
                );

                // The DEVICE plane starts the execution against that absent target.
                let mut start = json!({
                    "targetArn": target,
                    "commandArn": command,
                    "parameters": { "mode": { "S": "reboot" } },
                });
                if let Some(seconds) = case.timeout_seconds {
                    start["executionTimeoutSeconds"] = json!(seconds);
                }
                let (status, _, rendered, parsed) =
                    device("POST", "/command-executions".to_string(), start.to_string());
                assert_eq!(
                    status, 200,
                    "case {label:?}: StartCommandExecution against an absent target must \
                     answer 200, got {status} with {rendered}"
                );
                let execution_id = parsed["executionId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    !execution_id.is_empty(),
                    "case {label:?}: the start must mint an executionId, got {rendered}"
                );

                let read = |execution_id: &str, target_arn: &str| {
                    control(
                        "GET",
                        format!(
                            "/command-executions/{execution_id}?targetArn={}",
                            form_encode(target_arn)
                        ),
                        String::new(),
                    )
                };
                let delete = |execution_id: &str, target_arn: &str| {
                    control(
                        "DELETE",
                        format!(
                            "/command-executions/{execution_id}?targetArn={}",
                            form_encode(target_arn)
                        ),
                        String::new(),
                    )
                };

                // The CONTROL plane reports the execution the DEVICE plane minted, with
                // the ARN the start named echoed verbatim.
                let (status, _, rendered, reported) = read(&execution_id, &target);
                assert_eq!(
                    (
                        status,
                        reported["executionId"].as_str(),
                        reported["targetArn"].as_str(),
                        reported["commandArn"].as_str(),
                        reported["status"].as_str(),
                    ),
                    (
                        200,
                        Some(execution_id.as_str()),
                        Some(target.as_str()),
                        Some(command.as_str()),
                        Some("CREATED"),
                    ),
                    "case {label:?}: the execution of an absent target must be reported, \
                     got {status} with {rendered}"
                );
                assert_eq!(
                    (
                        reported["executionTimeoutSeconds"].as_i64(),
                        reported["parameters"]["mode"]["S"].as_str(),
                    ),
                    (case.timeout_seconds, Some("reboot")),
                    "case {label:?}: the reported timeout and parameters are the ones the \
                     start was given, got {rendered}"
                );

                // The single-execution reader and the LISTER answer about one record.
                let (status, _, rendered, listed) = control(
                    "POST",
                    "/command-executions".to_string(),
                    json!({ "targetArn": target }).to_string(),
                );
                let listed_ids: Vec<String> = listed["commandExecutions"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|summary| summary["executionId"].as_str().map(str::to_string))
                    .collect();
                assert_eq!(
                    (status, listed_ids.as_slice()),
                    (200, [execution_id.clone()].as_slice()),
                    "case {label:?}: the listing must project the same execution, got \
                     {status} with {rendered}"
                );

                // An `executionId` that device holds nothing under is the modelled
                // not-found answer, not a refusal of the target.
                let (status, error_code, rendered, _) = read("exec-absent-from-this-device", &target);
                assert_eq!(
                    (status, error_code.as_str()),
                    (404, "ResourceNotFoundException"),
                    "case {label:?}: an id this device holds nothing under is 404, got \
                     {status} with {rendered}"
                );

                if case.waits_for_the_timer {
                    // "A timer is started as soon as the command execution is created…it
                    // will automatically update to TIMED_OUT": the poll looks again until
                    // the transition the deadline owes is reported.
                    let started_looking = Instant::now();
                    let last = loop {
                        let (status, _, rendered, polled) = read(&execution_id, &target);
                        let seen = format!("{status} with {rendered}");
                        if status == 200 && polled["status"].as_str() == Some("TIMED_OUT") {
                            break None;
                        }
                        if started_looking.elapsed() >= POLL_DEADLINE {
                            break Some(seen);
                        }
                        std::thread::sleep(POLL_INTERVAL);
                    };
                    assert!(
                        last.is_none(),
                        "case {label:?}: an execution no device picked up must reach \
                         TIMED_OUT, last answer was {last:?}"
                    );
                }

                if case.device_reports_an_outcome {
                    for status_update in ["IN_PROGRESS", "SUCCEEDED"] {
                        let (status, _, rendered, _) = device(
                            "POST",
                            format!("/things/{thing}/jobs/{execution_id}"),
                            json!({ "status": status_update }).to_string(),
                        );
                        assert_eq!(
                            status, 200,
                            "case {label:?}: the device reporting {status_update} must \
                             answer 200, got {status} with {rendered}"
                        );
                    }
                    let (status, _, rendered, finished) = read(&execution_id, &target);
                    assert_eq!(
                        (status, finished["status"].as_str()),
                        (200, Some("SUCCEEDED")),
                        "case {label:?}: the control plane must report the status the \
                         device reported, got {status} with {rendered}"
                    );
                }

                // The DELETE resolves the same target: an execution a device reported an
                // outcome for is removed, and one in any other status is refused on its
                // DELETABILITY — never on the target.
                let (status, error_code, rendered, refusal) = delete(&execution_id, &target);
                if case.device_reports_an_outcome {
                    assert_eq!(
                        status, 200,
                        "case {label:?}: deleting the execution of an absent target must \
                         answer 200, got {status} with {rendered}"
                    );
                    let (status, error_code, rendered, _) = read(&execution_id, &target);
                    assert_eq!(
                        (status, error_code.as_str()),
                        (404, "ResourceNotFoundException"),
                        "case {label:?}: a deleted execution is no longer reported, got \
                         {status} with {rendered}"
                    );
                } else {
                    assert_eq!(
                        (status, error_code.as_str(), refusal["message"].as_str()),
                        (400, "ValidationException", Some(NOT_TERMINAL_MESSAGE)),
                        "case {label:?}: an execution no device reported an outcome for is \
                         refused on its deletability, got {status} with {rendered}"
                    );
                    let (status, _, rendered, _) = read(&execution_id, &target);
                    assert_eq!(
                        status, 200,
                        "case {label:?}: a refused delete leaves the execution readable, \
                         got {status} with {rendered}"
                    );
                }

                // What decides the target is still the ARN: the same device name under
                // ANOTHER account names no device of the caller.
                let outside = format!("arn:aws:iot:{REGION}:111122223333:thing/{thing}");
                let (status, _, rendered, refused) = read(&execution_id, &outside);
                assert_eq!(
                    (status, refused["Message"].as_str()),
                    (403, Some("targetArn not found in account")),
                    "case {label:?}: another account's ARN must stay the authorization \
                     refusal, got {status} with {rendered}"
                );

                // And no read, listing or delete of this suite registered the device.
                let registered = registered_things();
                assert!(
                    !registered.iter().any(|held| held == thing),
                    "case {label:?}: no command-execution call may register a thing, got \
                     {registered:?}"
                );
            }
        })
        .expect("the absent-target thread spawns")
        .join()
        .expect("the absent-target thread finishes");
}
