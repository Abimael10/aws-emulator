// SPDX-License-Identifier: CC0-1.0

//! What an application EXPLAINS its settled status with — `Application$StatusMessage`,
//! driven over the wire.
//!
//! Every case goes through the real router with the owner of the Amazon EC2 instances
//! mounted beside this service (the harness `tests/all/ec2_instance_shutdown.rs` owns, built
//! on the one `tests/all/application_lifecycle.rs` every suite of this target drives), so the
//! restJson1 routing, the boundary constraint walk, the discovery hop into EC2, the handler
//! and the response serialization all run exactly as they do in the emulator binary. Nothing
//! reaches into `crate::state`: an explanation is only believed when `GetApplication` reports
//! it back.
//!
//! RULE under test: `Application$StatusMessage` is "the status message"
//! (`smithy_query(query="doc", shape="Application", member="StatusMessage")`) of the status
//! the application is IN, so an application the clock has brought to an outcome the service
//! could not carry out reports the refusal that work ran into — the same sentence the record
//! of that work reports in `Operation$StatusMessage` — while an application whose outcome
//! explains itself, and one whose work is still running, report no `StatusMessage` at all.
//!
//! The rows span what the rule generalises over: whether the owner of the named instance
//! holds it, both lengths the modelled `InstanceId` form admits and a value that owner does
//! not read as an instance id at all, which moment the application is read at (while the work
//! is running and after it settled), and WHICH call delivers the outcome — the registration
//! that started the discovery, or a stop that takes over the delivery of the result that
//! discovery was going to report.
//!
//! Nothing here waits a fixed time: every span this suite crosses is a stored due instant and
//! nothing in the emulator sleeps for one, so a case LOOKS AGAIN until the condition holds or
//! [`SETTLE_DEADLINE`] passes, and no assertion is made about elapsed time.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_ssmsap_ssmsap::state::{
    APPLICATION_STATUS_ACTIVATED, APPLICATION_STATUS_FAILED, APPLICATION_STATUS_REGISTERING,
    APPLICATION_STATUS_STOPPED, APPLICATION_STATUS_STOPPING, DISCOVERY_STATUS_REGISTRATION_FAILED,
    DISCOVERY_STATUS_SUCCESS, OPERATION_STATUS_INPROGRESS,
};

use crate::ec2_instance_shutdown::{registration, Emulator as WithEc2, UNHELD_INSTANCE};

/// An id inside the modelled `InstanceId` form in its SHORT spelling
/// (`^i-[\w\d]{8}$|^i-[\w\d]{17}$`), which the owner of the account's instances holds no
/// instance for: nothing in this emulator is ever launched under an all-`f` counter value.
const UNHELD_SHORT_INSTANCE: &str = "i-ffffffff";

/// An id inside that same modelled form which the owner does not read as an instance id at
/// all, so it resolves to no instance either.
const UNREADABLE_INSTANCE: &str = "i-0zzzzzzzzzzzzzzzz";

/// How long a case is given for an outcome another clock owes.
///
/// Generous on purpose: the outcome is a stored due instant and nothing sleeps for one, so a
/// case looks again until it is reported. All this bound is for is that an outcome which never
/// comes fails instead of hanging; nothing asserts on elapsed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before reading again. Not a bound on anything.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// What `GetApplication` reports about an application's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    status: String,
    discovery_status: String,
    /// `Application$StatusMessage`, treating a JSON `null` as the absent member it is.
    status_message: Option<String>,
}

/// Read `application_id` back through `GetApplication`.
async fn read_application(emulator: &WithEc2, case: &str, application_id: &str) -> Reported {
    let reply = emulator
        .ssm_sap("/get-application", json!({"ApplicationId": application_id}))
        .await;
    let application = reply
        .get("Application")
        .unwrap_or_else(|| panic!("case {case:?}: expected an Application, got {reply}"));
    let member = |name: &str| {
        application
            .get(name)
            .filter(|value| !value.is_null())
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    Reported {
        status: member("Status")
            .unwrap_or_else(|| panic!("case {case:?}: expected an Application Status, got {reply}")),
        discovery_status: member("DiscoveryStatus").unwrap_or_else(|| {
            panic!("case {case:?}: expected an Application DiscoveryStatus, got {reply}")
        }),
        status_message: member("StatusMessage"),
    }
}

/// Read `application_id` back until it reports `status`, which is the outcome the clock owes
/// it; answer what it reported then.
async fn read_when(
    emulator: &WithEc2,
    case: &str,
    application_id: &str,
    status: &str,
) -> Reported {
    let until = Instant::now() + SETTLE_DEADLINE;
    loop {
        let reported = read_application(emulator, case, application_id).await;
        if reported.status == status {
            return reported;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected the application to reach {status}, it reports {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `Operation$StatusMessage` the record `operation_id` reports, once that record has
/// reached a terminal status — the explanation of the very work the application's status
/// came out of.
async fn record_explanation(emulator: &WithEc2, case: &str, operation_id: &str) -> Option<String> {
    let until = Instant::now() + SETTLE_DEADLINE;
    loop {
        let reply = emulator
            .ssm_sap("/get-operation", json!({"OperationId": operation_id}))
            .await;
        let operation = reply
            .get("Operation")
            .unwrap_or_else(|| panic!("case {case:?}: expected an Operation, got {reply}"));
        let status = operation
            .get("Status")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected an Operation Status, got {reply}"));
        if status != OPERATION_STATUS_INPROGRESS {
            return operation
                .get("StatusMessage")
                .filter(|value| !value.is_null())
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected the record {operation_id} to settle, it reports {reply}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Register `application_id` on `instance` and answer the `OperationId` the call filed.
async fn register(emulator: &WithEc2, case: &str, application_id: &str, instance: &str) -> String {
    let reply = emulator
        .ssm_sap(
            "/register-application",
            registration(application_id, instance, &[]),
        )
        .await;
    reply
        .get("OperationId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an OperationId, got {reply}"))
        .to_string()
}

/// One registration, and the outcome the discovery it runs on the instance it names leaves it
/// with.
#[derive(Debug)]
struct Row {
    case: &'static str,
    /// `None` is the instance this suite launches through EC2 itself, so the owner holds it
    /// and reports it running; `Some` is a value that owner resolves to no instance.
    instance: Option<&'static str>,
    /// The `ApplicationStatus` the registration settles as.
    status: &'static str,
    /// The `ApplicationDiscoveryStatus` it settles as.
    discovery_status: &'static str,
    /// Whether the settled application has to EXPLAIN that outcome.
    explains: bool,
}

/// The rows: an instance the owner holds against each way a named instance resolves to none.
const ROWS: &[Row] = &[
    Row {
        case: "an instance the owner holds and reports running",
        instance: None,
        status: APPLICATION_STATUS_ACTIVATED,
        discovery_status: DISCOVERY_STATUS_SUCCESS,
        explains: false,
    },
    Row {
        case: "a long-form id the owner holds no instance for",
        instance: Some(UNHELD_INSTANCE),
        status: APPLICATION_STATUS_FAILED,
        discovery_status: DISCOVERY_STATUS_REGISTRATION_FAILED,
        explains: true,
    },
    Row {
        case: "a short-form id the owner holds no instance for",
        instance: Some(UNHELD_SHORT_INSTANCE),
        status: APPLICATION_STATUS_FAILED,
        discovery_status: DISCOVERY_STATUS_REGISTRATION_FAILED,
        explains: true,
    },
    Row {
        case: "an id the owner does not read as an instance id at all",
        instance: Some(UNREADABLE_INSTANCE),
        status: APPLICATION_STATUS_FAILED,
        discovery_status: DISCOVERY_STATUS_REGISTRATION_FAILED,
        explains: true,
    },
];

/// RULE: a settled application explains a status the service could not carry out with the
/// refusal that work ran into, and explains nothing where the status explains itself.
#[tokio::test]
async fn a_settled_application_explains_the_status_the_clock_brought_it_to() {
    let emulator = WithEc2::new();
    let launched = emulator.launch_running(1).await[0].clone();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let application_id = format!("explained-{index}");
        let instance = row.instance.unwrap_or(launched.as_str());
        let operation = register(&emulator, case, &application_id, instance).await;

        // While the registration is still running there is nothing to explain, whatever it
        // is going to settle as.
        let running = read_application(&emulator, case, &application_id).await;
        if running.status == APPLICATION_STATUS_REGISTERING {
            assert_eq!(
                running.status_message, None,
                "case {case:?}: a registration still running explains nothing, got {running:?}"
            );
        }

        let settled = read_when(&emulator, case, &application_id, row.status).await;
        assert_eq!(
            settled.discovery_status, row.discovery_status,
            "case {case:?}: expected the discovery result {}, got {settled:?}",
            row.discovery_status
        );
        assert_eq!(
            settled.status_message.is_some(),
            row.explains,
            "case {case:?}: expected an explanation = {}, got {settled:?}",
            row.explains
        );

        // The explanation is the one the service computed for THIS work, so it is the very
        // sentence the record of that work reports — and it names the instance the discovery
        // could not be carried out on.
        let explanation = record_explanation(&emulator, case, &operation).await;
        assert_eq!(
            settled.status_message, explanation,
            "case {case:?}: expected the application to explain its status with the sentence \
             its own operation record reports ({explanation:?}), got {settled:?}"
        );
        if let Some(relayed) = &settled.status_message {
            assert!(
                relayed.contains(instance),
                "case {case:?}: expected the explanation to name {instance}, got {relayed:?}"
            );
        }

        // A read changes nothing a later caller sees: the explanation is the application's
        // state rather than something the read computed.
        let again = read_application(&emulator, case, &application_id).await;
        assert_eq!(
            again, settled,
            "case {case:?}: expected a second read to report the same outcome, got {again:?}"
        );
    }
}

/// RULE, where another call delivers the outcome: a stop takes over the delivery of the
/// discovery result the registration it overtook was going to report, so it carries that
/// result's explanation with it — and a stop of an application whose registration succeeded
/// carries none, because that outcome explains itself.
#[tokio::test]
async fn a_stop_carries_the_explanation_of_the_result_it_takes_over() {
    let emulator = WithEc2::new();
    let launched = emulator.launch_running(1).await[0].clone();

    // (case, the instance the registration names, the discovery result the stop delivers,
    //  whether the stopped application has to explain itself)
    let rows: &[(&str, &str, &str, bool)] = &[
        (
            "a stop overtaking a registration whose discovery cannot be carried out",
            UNHELD_INSTANCE,
            DISCOVERY_STATUS_REGISTRATION_FAILED,
            true,
        ),
        (
            "a stop of an application whose discovery was carried out",
            launched.as_str(),
            DISCOVERY_STATUS_SUCCESS,
            false,
        ),
    ];

    for (index, (case, instance, discovery_status, explains)) in rows.iter().enumerate() {
        let application_id = format!("stopped-{index}");
        register(&emulator, case, &application_id, instance).await;

        let stopped = emulator
            .ssm_sap("/stop-application", json!({"ApplicationId": &application_id}))
            .await;
        assert!(
            stopped
                .get("OperationId")
                .and_then(Value::as_str)
                .is_some(),
            "case {case:?}: a stop hands out the record of its own work, got {stopped}"
        );
        // The stop is accepted while the registration runs, so the application is STOPPING
        // and has nothing to explain until the stop's own outcome falls due.
        let stopping = read_application(&emulator, case, &application_id).await;
        if stopping.status == APPLICATION_STATUS_STOPPING {
            assert_eq!(
                stopping.status_message, None,
                "case {case:?}: a stop still running explains nothing, got {stopping:?}"
            );
        }

        let settled = read_when(&emulator, case, &application_id, APPLICATION_STATUS_STOPPED).await;
        assert_eq!(
            settled.discovery_status, *discovery_status,
            "case {case:?}: expected the stop to deliver the discovery result \
             {discovery_status}, got {settled:?}"
        );
        assert_eq!(
            settled.status_message.is_some(),
            *explains,
            "case {case:?}: expected an explanation = {explains}, got {settled:?}"
        );
        if let Some(relayed) = &settled.status_message {
            assert!(
                relayed.contains(*instance),
                "case {case:?}: expected the explanation to name {instance}, got {relayed:?}"
            );
        }
    }
}
