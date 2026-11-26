// SPDX-License-Identifier: CC0-1.0

//! What a REFRESH of a registered application settles as, with the owner of its Amazon EC2
//! instances mounted beside this service.
//!
//! Every case goes through the real router (the harness `tests/all/application_lifecycle.rs`
//! owns, mounted with EC2 beside this service through the seam
//! `tests/all/ec2_instance_shutdown.rs` owns), so the restJson1 routing, the boundary
//! constraint validation, the handler and the response serialization all run exactly as
//! they do in the emulator binary. Nothing reaches into `crate::state`: a refresh's outcome
//! is only believed when `GetApplication`, `GetOperation` and `ListComponents` report it
//! back, and the state of an instance is only believed when EC2 itself reports it.
//!
//! RULE under test: a call that re-runs a registered application's DISCOVERY settles as
//! what that discovery can do on the application's Amazon EC2 instances — the same
//! Systems Manager document run on each of them that the registration's own discovery
//! made, so the same question gets the same answer whichever call asks it. A re-discovery
//! the owner of those instances lets the service carry out leaves the application's latest
//! discovery result `SUCCESS` with nothing to explain and its operation record `SUCCESS`;
//! one it cannot leaves that result `REFRESH_FAILED` — the model's own discovery result for
//! a failed refresh, as opposed to the failed REGISTRATION's — explaining itself with the
//! refusal the inner call answered, and the record `ERROR` carrying the same sentence. The
//! registration itself stands either way: the application keeps its own `ApplicationStatus`
//! and the components that registration discovered, so a later refresh of the same
//! application reports `SUCCESS` again once its instance can take a document.
//!
//! The rows span what the rule generalises over: the state the owner reports the
//! application's instance in (running, stopped, terminated), the route it was brought into
//! that state by (this service's own stop with the instance shutdown, and EC2's own
//! `StopInstances`), and the `ApplicationStatus` the application is in when the refresh is
//! asked for.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_ssmsap_ssmsap::ops::types::APPLICATION_DISCOVERY_STATUS_VALUES;
use service_com_amazonaws_ssmsap_ssmsap::state::{
    DISCOVERY_STATUS_REFRESH_FAILED, DISCOVERY_STATUS_REGISTERING, DISCOVERY_STATUS_SUCCESS,
    TRANSITION_SPAN,
};

use crate::ec2_instance_shutdown::{registration, Emulator as WithEc2};
use crate::registration_outcome::{discovery_refusal, without_request_id};

/// `OperationStatus$INPROGRESS`, as the model spells it: the status a record carries until
/// the clock brings its outcome due.
const OPERATION_STATUS_INPROGRESS: &str = "INPROGRESS";

/// `OperationStatus$SUCCESS`, as the model spells it.
const OPERATION_STATUS_SUCCESS: &str = "SUCCESS";

/// `OperationStatus$ERROR`, as the model spells it.
const OPERATION_STATUS_ERROR: &str = "ERROR";

/// How long a condition the clock owes — a status this service owes an application, a state
/// EC2 owes an instance — is given to come due.
///
/// Generous on purpose: nothing in the emulator sleeps, so every wait below polls for the
/// CONDITION and this is only the deadline that stops a broken build polling forever. It
/// pins nothing about how fast the host runs.
fn settle_budget() -> Duration {
    TRANSITION_SPAN + Duration::from_secs(60)
}

/// How long each poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The `Application` member `name` of `id`, as `GetApplication` reports it.
async fn application_member(emulator: &WithEc2, id: &str, name: &str) -> Option<String> {
    let reply = emulator
        .ssm_sap("/get-application", json!({"ApplicationId": id}))
        .await;
    reply
        .get("Application")
        .and_then(|application| application.get(name))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The `(Status, DiscoveryStatus, StatusMessage)` of `id`.
async fn application_outcome(
    emulator: &WithEc2,
    case: &str,
    id: &str,
) -> (String, String, Option<String>) {
    let reply = emulator
        .ssm_sap("/get-application", json!({"ApplicationId": id}))
        .await;
    let member = |name: &str| {
        reply
            .get("Application")
            .and_then(|application| application.get(name))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let status = member("Status")
        .unwrap_or_else(|| panic!("case {case:?}: expected an Application Status, got {reply}"));
    let discovery_status = member("DiscoveryStatus").unwrap_or_else(|| {
        panic!("case {case:?}: expected an Application DiscoveryStatus, got {reply}")
    });
    (status, discovery_status, member("StatusMessage"))
}

/// Wait until the discovery of `id` is no longer running, and answer what it settled as.
///
/// Polls the condition rather than a duration: the next discovery result is due one
/// `TRANSITION_SPAN` after the call that started it whether or not anybody asks, so this
/// looks again until the application reports it.
async fn settled_discovery(
    emulator: &WithEc2,
    case: &str,
    id: &str,
) -> (String, String, Option<String>) {
    let deadline = Instant::now() + settle_budget();
    loop {
        let outcome = application_outcome(emulator, case, id).await;
        if outcome.1 != DISCOVERY_STATUS_REGISTERING {
            return outcome;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the discovery of {id} must settle, it still reports {outcome:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until `id` reports `want` as its `ApplicationStatus`, so the change the clock owed
/// it has come due and no work is in flight on it.
async fn await_application_status(emulator: &WithEc2, case: &str, id: &str, want: &str) {
    let deadline = Instant::now() + settle_budget();
    loop {
        let status = application_member(emulator, id, "Status").await;
        if status.as_deref() == Some(want) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {id} must reach {want}, it reports {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until EC2 reports `instance` in `want`.
async fn await_instance_state(emulator: &WithEc2, case: &str, instance: &str, want: &str) {
    let ids = [instance.to_string()];
    let deadline = Instant::now() + settle_budget();
    loop {
        let states = emulator.instance_states(&ids).await;
        if states.get(instance).map(String::as_str) == Some(want) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: EC2 must report {instance} {want}, it reports {states:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until the record `operation_id` has reached its terminal status, and answer
/// `(Status, StatusMessage)`.
async fn settled_operation(
    emulator: &WithEc2,
    case: &str,
    operation_id: &str,
) -> (String, Option<String>) {
    let deadline = Instant::now() + settle_budget();
    loop {
        let reply = emulator
            .ssm_sap("/get-operation", json!({"OperationId": operation_id}))
            .await;
        let operation = reply
            .get("Operation")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected an Operation, got {reply}"));
        let status = operation
            .get("Status")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected an Operation Status, got {reply}"))
            .to_string();
        if status != OPERATION_STATUS_INPROGRESS {
            let status_message = operation
                .get("StatusMessage")
                .and_then(Value::as_str)
                .map(str::to_string);
            return (status, status_message);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the record {operation_id} must settle, it reports {status}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `ComponentId`s `ListComponents` reports for `id`, sorted.
async fn listed_components(emulator: &WithEc2, id: &str) -> Vec<String> {
    let reply = emulator
        .ssm_sap("/list-components", json!({"ApplicationId": id}))
        .await;
    let mut ids: Vec<String> = reply
        .get("Components")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("ComponentId").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

/// Register `id` on `instance` and wait until its registration has settled as discovered —
/// the application state a refresh has something to re-discover for.
async fn register_discovered(emulator: &WithEc2, case: &str, id: &str, instance: &str) {
    emulator
        .ssm_sap("/register-application", registration(id, instance, &[]))
        .await;
    let settled = settled_discovery(emulator, case, id).await;
    assert_eq!(
        (settled.0.as_str(), settled.1.as_str()),
        ("ACTIVATED", DISCOVERY_STATUS_SUCCESS),
        "case {case:?}: setup expected a discovered application, got {settled:?}"
    );
}

/// Ask for a refresh of `id` and answer the `OperationId` it filed.
async fn refresh(emulator: &WithEc2, case: &str, id: &str) -> String {
    let reply = emulator
        .ssm_sap("/start-application-refresh", json!({"ApplicationId": id}))
        .await;
    reply
        .get("OperationId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an OperationId, got {reply}"))
        .to_string()
}

/// How the row's instance is brought out of `running` before the refresh is asked for.
#[derive(Debug, Clone, Copy)]
enum Before {
    /// Nothing at all: EC2 reports the instance running.
    Running,
    /// This service's own route into EC2: a stop that asks for the associated instance to
    /// be shut down with the application.
    ApplicationStopWithShutdown,
    /// EC2's own `StopInstances`, so the same instance state is reached by another route.
    Ec2Stop,
    /// EC2's own `TerminateInstances`: a state an instance never leaves again.
    Ec2Terminate,
}

/// One refresh, and what the re-discovery it runs on the application's instance can do.
#[derive(Debug)]
struct Row {
    case: &'static str,
    /// The `ApplicationId` the row registers, spanning the forms the modelled
    /// `^[\w\d\.-]+$` admits.
    id: &'static str,
    before: Before,
    /// The `ApplicationStatus` the application is in when the refresh is asked for — and,
    /// because a refresh re-runs discovery alone, the one it keeps.
    status: &'static str,
    /// The `ApplicationDiscoveryStatus` the refresh settles as.
    discovery_status: &'static str,
    /// The `OperationStatus` the record the refresh filed settles as.
    operation_status: &'static str,
}

/// The rows: the state EC2 reports the application's instance in when the refresh is asked
/// for, by each route that state is reached through.
const ROWS: &[Row] = &[
    Row {
        case: "an instance the owner reports running",
        id: "refresh-running",
        before: Before::Running,
        status: "ACTIVATED",
        discovery_status: DISCOVERY_STATUS_SUCCESS,
        operation_status: OPERATION_STATUS_SUCCESS,
    },
    Row {
        case: "an instance this service's own stop shut down",
        id: "refresh.stopped-by-stop",
        before: Before::ApplicationStopWithShutdown,
        status: "STOPPED",
        discovery_status: DISCOVERY_STATUS_REFRESH_FAILED,
        operation_status: OPERATION_STATUS_ERROR,
    },
    Row {
        case: "an instance its owner stopped",
        id: "r",
        before: Before::Ec2Stop,
        status: "ACTIVATED",
        discovery_status: DISCOVERY_STATUS_REFRESH_FAILED,
        operation_status: OPERATION_STATUS_ERROR,
    },
    Row {
        case: "an instance its owner terminated",
        id: "REFRESH_TERMINATED",
        before: Before::Ec2Terminate,
        status: "ACTIVATED",
        discovery_status: DISCOVERY_STATUS_REFRESH_FAILED,
        operation_status: OPERATION_STATUS_ERROR,
    },
];

/// RULE: a refresh settles as the discovery it re-runs on the application's instance could,
/// and the record it filed carries the refusal that re-discovery ran into — while the
/// application keeps its own status and the components its registration discovered.
#[tokio::test]
async fn a_refresh_settles_as_its_rediscovery_of_the_registered_instance_could() {
    // The discovery results a refresh reports are the model's own
    // (`smithy_query(query="shape", shape="ApplicationDiscoveryStatus")`), so no row can
    // pass on a value the API does not define.
    for discovery_status in [DISCOVERY_STATUS_SUCCESS, DISCOVERY_STATUS_REFRESH_FAILED] {
        assert!(
            APPLICATION_DISCOVERY_STATUS_VALUES.contains(&discovery_status),
            "the discovery results must be modelled values, got {discovery_status:?}"
        );
    }

    for row in ROWS {
        let case = row.case;
        let emulator = WithEc2::new();
        let instance = emulator.launch_running(1).await[0].clone();
        register_discovered(&emulator, case, row.id, &instance).await;
        let components_before = listed_components(&emulator, row.id).await;
        assert!(
            !components_before.is_empty(),
            "case {case:?}: setup expected a discovered application to have components"
        );

        match row.before {
            Before::Running => {}
            Before::ApplicationStopWithShutdown => {
                emulator
                    .ssm_sap(
                        "/stop-application",
                        json!({"ApplicationId": row.id, "IncludeEc2InstanceShutdown": true}),
                    )
                    .await;
                await_application_status(&emulator, case, row.id, "STOPPED").await;
                await_instance_state(&emulator, case, &instance, "stopped").await;
            }
            Before::Ec2Stop => {
                emulator
                    .ec2()
                    .stop_instances()
                    .instance_ids(&instance)
                    .send()
                    .await
                    .unwrap_or_else(|error| {
                        panic!("case {case:?}: EC2 must stop {instance}: {error:?}")
                    });
                await_instance_state(&emulator, case, &instance, "stopped").await;
            }
            Before::Ec2Terminate => {
                emulator
                    .ec2()
                    .terminate_instances()
                    .instance_ids(&instance)
                    .send()
                    .await
                    .unwrap_or_else(|error| {
                        panic!("case {case:?}: EC2 must terminate {instance}: {error:?}")
                    });
                await_instance_state(&emulator, case, &instance, "terminated").await;
            }
        }

        // The status the application is in when the refresh is asked for is the one the
        // row expects it to keep.
        let before = application_outcome(&emulator, case, row.id).await;
        assert_eq!(
            before.0, row.status,
            "case {case:?}: expected the application in {}, got {before:?}",
            row.status
        );

        let record = refresh(&emulator, case, row.id).await;
        let (status, discovery_status, status_message) =
            settled_discovery(&emulator, case, row.id).await;
        assert_eq!(
            (status.as_str(), discovery_status.as_str()),
            (row.status, row.discovery_status),
            "case {case:?}: expected {}/{}, got {status}/{discovery_status}",
            row.status,
            row.discovery_status
        );

        // What the application explains its latest discovery result with: the refusal the
        // re-discovery ran into, and nothing for a result that explains itself.
        match (&status_message, row.operation_status) {
            (None, OPERATION_STATUS_SUCCESS) => {}
            (Some(relayed), OPERATION_STATUS_ERROR) => assert_eq!(
                without_request_id(case, relayed),
                discovery_refusal(&instance),
                "case {case:?}: expected the relayed refusal, got {relayed:?}"
            ),
            _ => panic!(
                "case {case:?}: a failed re-discovery explains itself and a successful one \
                 does not, got {status_message:?} for {}",
                row.operation_status
            ),
        }

        let (operation_status, operation_message) =
            settled_operation(&emulator, case, &record).await;
        assert_eq!(
            operation_status, row.operation_status,
            "case {case:?}: expected the record to settle {}, got {operation_status}",
            row.operation_status
        );
        assert_eq!(
            operation_message.as_ref().map(|relayed| without_request_id(case, relayed)),
            status_message.as_ref().map(|relayed| without_request_id(case, relayed)),
            "case {case:?}: the record explains the outcome the application reached, got \
             {operation_message:?}"
        );

        // A refresh re-runs the discovery of a registration that stands, so whatever that
        // re-discovery came to, the application keeps the components the registration
        // discovered.
        assert_eq!(
            listed_components(&emulator, row.id).await,
            components_before,
            "case {case:?}: the re-discovery changed no component"
        );
    }
}

/// RULE: a failed re-discovery is the application's LATEST discovery result and nothing
/// more — the registration stands, so the identical refresh reports `SUCCESS` again once
/// the owner of the instance reports it able to take a document, and the sentence the
/// failed one explained itself with is gone.
#[tokio::test]
async fn a_refresh_reports_success_again_once_the_instance_can_take_a_document() {
    let case = "an instance stopped and started again";
    let id = "refresh-recovers";
    let emulator = WithEc2::new();
    let instance = emulator.launch_running(1).await[0].clone();
    register_discovered(&emulator, case, id, &instance).await;

    // Stopped: the re-discovery cannot be carried out.
    emulator
        .ec2()
        .stop_instances()
        .instance_ids(&instance)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: EC2 must stop {instance}: {error:?}"));
    await_instance_state(&emulator, case, &instance, "stopped").await;
    let failed_record = refresh(&emulator, case, id).await;
    let failed = settled_discovery(&emulator, case, id).await;
    assert_eq!(
        failed.1, DISCOVERY_STATUS_REFRESH_FAILED,
        "case {case:?}: a refresh over a stopped instance fails, got {failed:?}"
    );
    let (failed_status, _) = settled_operation(&emulator, case, &failed_record).await;
    assert_eq!(
        failed_status, OPERATION_STATUS_ERROR,
        "case {case:?}: the failed refresh's record is in error, got {failed_status}"
    );

    // Running again: the identical call reports the latest discovery result it now gets.
    emulator
        .ec2()
        .start_instances()
        .instance_ids(&instance)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: EC2 must start {instance}: {error:?}"));
    await_instance_state(&emulator, case, &instance, "running").await;
    let recovered_record = refresh(&emulator, case, id).await;
    let recovered = settled_discovery(&emulator, case, id).await;
    assert_eq!(
        (recovered.1.as_str(), recovered.2.as_ref()),
        (DISCOVERY_STATUS_SUCCESS, None),
        "case {case:?}: the re-discovery succeeds and explains nothing, got {recovered:?}"
    );
    let (recovered_status, recovered_message) =
        settled_operation(&emulator, case, &recovered_record).await;
    assert_eq!(
        (recovered_status.as_str(), recovered_message.as_ref()),
        (OPERATION_STATUS_SUCCESS, None),
        "case {case:?}: the record of the successful refresh explains nothing, got \
         {recovered_message:?}"
    );
    assert!(
        !listed_components(&emulator, id).await.is_empty(),
        "case {case:?}: the application still has the components its registration discovered"
    );
}
