// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the NETWORK ATTACHMENTS a MicroVM reports, and for the reason a
//! customer-requested termination reports, driven over the wire through the same router
//! the emulator boots.
//!
//! RULE: a MicroVM's `ingressNetworkConnectors` and `egressNetworkConnectors` are
//! properties of the MicroVM — the network paths it actually has — so every response
//! reporting one (the launch and every later read) reports the connectors it is attached
//! to: the ones the request named, or the AWS-managed connector of that direction when
//! the request named none, unchanged for the MicroVM's whole lifecycle.
//!
//! Each test states that rule and exercises it over a SET of inputs: the rows differ in
//! which direction the request names, in how many connectors it names, and in the run
//! state the MicroVM is read in. The harness (`emulator`, `call`, `built_image`,
//! `run_microvm`, …) is the one the lifecycle suite beside this file owns.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::microvm_lifecycle::{
    as_uri_value, built_image, call, emulator, poll_until_microvm_state, run_microvm, REGION,
};

/// The AWS-managed INGRESS connector a MicroVM that names none is attached to, in the ARN
/// form an AWS-OWNED resource carries (`aws` in the account field rather than this
/// account's id). Source: the recorded `RunMicrovm`/`GetMicrovm` 200 of a MicroVM run
/// with no `ingressNetworkConnectors`.
fn managed_ingress() -> String {
    format!("arn:aws:lambda:{REGION}:aws:network-connector:aws-network-connector:HTTP_INGRESS")
}

/// The AWS-managed EGRESS connector a MicroVM that names none is attached to, in the same
/// AWS-owned ARN form and from the same recorded answers as [`managed_ingress`].
fn managed_egress() -> String {
    format!("arn:aws:lambda:{REGION}:aws:network-connector:aws-network-connector:INTERNET_EGRESS")
}

/// A customer-owned connector ARN, the other form `NetworkConnector` (len 1..2048)
/// accepts: a resource of this account's own namespace.
fn own_connector(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:000000000000:network-connector:{name}")
}

/// Read the stored MicroVM back through `GetMicrovm`.
async fn get_microvm(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/2025-09-09/microvms/{}", as_uri_value(identifier)),
        None,
    )
    .await
}

/// Ask `asked` (`suspend` or `resume`) of the MicroVM `identifier` names.
async fn transition(router: &MultiServiceRouter, asked: &str, identifier: &str) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/2025-09-09/microvms/{}/{asked}", as_uri_value(identifier)),
        None,
    )
    .await
}

/// Terminate the MicroVM `identifier` names.
async fn terminate(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    call(
        router,
        "DELETE",
        &format!("/2025-09-09/microvms/{}", as_uri_value(identifier)),
        None,
    )
    .await
}

/// Assert that `answer` reports exactly the connector lists `ingress` and `egress`.
fn assert_connectors(answer: &Value, case: &str, ingress: &[String], egress: &[String]) {
    for (member, expected) in [
        ("ingressNetworkConnectors", ingress),
        ("egressNetworkConnectors", egress),
    ] {
        let actual: Vec<String> = answer[member]
            .as_array()
            .map(|list| {
                list.iter()
                    .map(|value| value.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {member} {expected:?}, got {:?}",
            answer[member]
        );
    }
}

/// One row: what a failing assertion calls it, the connector members the request carries,
/// and the `ingressNetworkConnectors` / `egressNetworkConnectors` the MicroVM must report.
type AttachmentRow = (&'static str, Value, Vec<String>, Vec<String>);

/// The rows the attachment assertions span: which direction the request names, and how
/// many connectors it names in it.
fn attachment_rows() -> Vec<AttachmentRow> {
    vec![
        (
            "neither direction named",
            json!({}),
            vec![managed_ingress()],
            vec![managed_egress()],
        ),
        (
            "an empty list in each direction",
            json!({"ingressNetworkConnectors": [], "egressNetworkConnectors": []}),
            vec![managed_ingress()],
            vec![managed_egress()],
        ),
        (
            "one ingress connector named, no egress",
            json!({"ingressNetworkConnectors": [own_connector("ingress-one")]}),
            vec![own_connector("ingress-one")],
            vec![managed_egress()],
        ),
        (
            "one egress connector named, no ingress",
            json!({"egressNetworkConnectors": [own_connector("egress-one")]}),
            vec![managed_ingress()],
            vec![own_connector("egress-one")],
        ),
        (
            "several connectors named in both directions",
            json!({
                "ingressNetworkConnectors": [
                    own_connector("ingress-a"),
                    own_connector("ingress-b"),
                    own_connector("ingress-c"),
                ],
                "egressNetworkConnectors": [
                    own_connector("egress-a"),
                    own_connector("egress-b"),
                ],
            }),
            vec![
                own_connector("ingress-a"),
                own_connector("ingress-b"),
                own_connector("ingress-c"),
            ],
            vec![own_connector("egress-a"), own_connector("egress-b")],
        ),
    ]
}

/// One launched MicroVM of the attachment table: the case, the MicroVM's id, the launch's
/// own answer, and the connector lists it must report.
type LaunchedRow = (&'static str, String, Value, Vec<String>, Vec<String>);

/// RULE: the launch reports the connectors the MicroVM is attached to — the ones the
/// request named, or the AWS-managed connector of a direction the request named none in —
/// and the read reports the same stored lists, before and after the provisioning the
/// clock owes.
#[tokio::test]
async fn a_microvm_reports_the_connectors_it_is_attached_to_on_the_launch_and_the_read() {
    let router = emulator();
    built_image(&router, "attachable").await;

    // Every row is launched first, so the one wait below covers all of them.
    let mut launched: Vec<LaunchedRow> = Vec::new();
    for (case, extra, ingress, egress) in attachment_rows() {
        let mut body = json!({"imageIdentifier": "attachable"});
        if let (Value::Object(members), Value::Object(map)) = (extra, &mut body) {
            for (key, value) in members {
                map.insert(key, value);
            }
        }
        let (status, run) = run_microvm(&router, body).await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {status} {run}");
        assert_connectors(&run, case, &ingress, &egress);

        let microvm_id = run["microvmId"].as_str().unwrap_or_default().to_string();
        // The record the launch persisted, read straight back: still PENDING.
        let (status, pending) = get_microvm(&router, &microvm_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the MicroVM reads back, got {status} {pending}"
        );
        assert_eq!(
            pending, run,
            "case {case:?}: GetMicrovm must return the record RunMicrovm stored"
        );
        launched.push((case, microvm_id, run, ingress, egress));
    }

    for (case, microvm_id, run, ingress, egress) in launched {
        // The read waits for the CONDITION the launch owes — the MicroVM reporting
        // `RUNNING` — rather than for any length of time.
        poll_until_microvm_state(&router, case, &microvm_id, "RUNNING").await;
        let (status, running) = get_microvm(&router, &microvm_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the MicroVM reads back, got {status} {running}"
        );
        assert_eq!(
            running["state"].as_str(),
            Some("RUNNING"),
            "case {case:?}: the provisioning the clock made due is materialised, got {:?}",
            running["state"]
        );
        assert_connectors(&running, case, &ingress, &egress);
        // The attachment is a property of the MicroVM, so the launch and the read of a
        // provisioned MicroVM report one and the same lists.
        for member in ["ingressNetworkConnectors", "egressNetworkConnectors"] {
            assert_eq!(
                running[member], run[member],
                "case {case:?}: {member} must not change with the run state, got {} and {}",
                running[member], run[member]
            );
        }
    }
}

/// RULE: a connector attachment does not change across the MicroVM's lifecycle, so every
/// read reports the same lists in every run state — suspending, resuming and terminating
/// the MicroVM included.
#[tokio::test]
async fn the_connectors_a_microvm_reports_survive_its_whole_lifecycle() {
    let router = emulator();
    built_image(&router, "lifecycle-attached").await;

    // The recorded case: a launch that names no connector at all, so every read must
    // report the AWS-managed pair.
    let (status, run) = run_microvm(&router, json!({"imageIdentifier": "lifecycle-attached"})).await;
    assert_eq!(status, 200, "run: expected 200, got {status} {run}");
    let microvm_id = run["microvmId"].as_str().unwrap_or_default().to_string();
    let ingress = vec![managed_ingress()];
    let egress = vec![managed_egress()];
    assert_connectors(&run, "the launch", &ingress, &egress);

    // Each row: the case, what to ask of the MicroVM first (`None` for a plain read),
    // whether the state the row expects is one a call makes DUE on the clock (so the read
    // waits for that condition) or one the call reaches at once, and the state the read
    // must report. The rows walk the whole lifecycle in order.
    let rows: Vec<(&str, Option<&str>, bool, &str)> = vec![
        ("straight after the launch", None, false, "PENDING"),
        ("once provisioning is due", None, true, "RUNNING"),
        ("while the suspend is in flight", Some("suspend"), false, "SUSPENDING"),
        ("once the suspend is due", None, true, "SUSPENDED"),
        ("resumed again", Some("resume"), true, "RUNNING"),
        ("while the termination is in flight", Some("terminate"), false, "TERMINATING"),
        ("once the termination is due", None, true, "TERMINATED"),
    ];

    for (case, asked, owed_by_the_clock, expected_state) in rows {
        match asked {
            Some("terminate") => {
                let (status, body) = terminate(&router, &microvm_id).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: TerminateMicrovm: expected 200, got {status} {body}"
                );
            }
            Some(other) => {
                let (status, body) = transition(&router, other, &microvm_id).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: {other}: expected 200, got {status} {body}"
                );
            }
            None => {}
        }
        if owed_by_the_clock {
            poll_until_microvm_state(&router, case, &microvm_id, expected_state).await;
        }

        let (status, got) = get_microvm(&router, &microvm_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the MicroVM reads back, got {status} {got}"
        );
        assert_eq!(
            got["state"].as_str(),
            Some(expected_state),
            "case {case:?}: expected state {expected_state}, got {:?}",
            got["state"]
        );
        assert_connectors(&got, case, &ingress, &egress);
    }
}

/// RULE: a MicroVM the customer asked to terminate is terminated cleanly, so every read
/// of it reports the outcome of that work as its `stateReason` — whatever run state the
/// MicroVM was in when the termination was asked for, and while the termination is in
/// flight as well as once it is done.
#[tokio::test]
async fn a_customer_requested_termination_reports_the_successful_outcome_as_its_reason() {
    let router = emulator();
    built_image(&router, "terminable-states").await;

    // The reason AWS reports for a clean, successful customer-requested termination:
    // whole fixed text, no variable part. Source: the recorded `GetMicrovm` 200 of a
    // MicroVM terminated through `TerminateMicrovm`.
    const SUCCESSFUL_TERMINATION: &str = "Success.";

    // Rows: the run state the MicroVM is driven to before it is terminated — the whole
    // space of non-terminal states a termination can be asked from.
    let rows: Vec<(&str, Option<&str>)> = vec![
        ("terminated while still PENDING", None),
        ("terminated while RUNNING", Some("RUNNING")),
        ("terminated while SUSPENDED", Some("SUSPENDED")),
    ];

    for (case, seeded) in rows {
        let (status, run) =
            run_microvm(&router, json!({"imageIdentifier": "terminable-states"})).await;
        assert_eq!(status, 200, "case {case:?}: run: got {status} {run}");
        let microvm_id = run["microvmId"].as_str().unwrap_or_default().to_string();
        assert!(
            run["stateReason"].is_null(),
            "case {case:?}: a launched MicroVM reports no stateReason, got {:?}",
            run["stateReason"]
        );

        if let Some(state) = seeded {
            // Each seeded state is reached the way a client reaches it: the call that
            // starts the move, then a look for the state it owes.
            poll_until_microvm_state(&router, case, &microvm_id, "RUNNING").await;
            if state == "SUSPENDED" {
                let (status, body) = transition(&router, "suspend", &microvm_id).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: SuspendMicrovm: got {status} {body}"
                );
            }
            poll_until_microvm_state(&router, case, &microvm_id, state).await;
            let (status, seed) = get_microvm(&router, &microvm_id).await;
            assert_eq!(status, 200, "case {case:?}: read: got {status} {seed}");
            assert_eq!(
                seed["state"].as_str(),
                Some(state),
                "case {case:?}: the MicroVM must really be in that state, got {:?}",
                seed["state"]
            );
        }

        let (status, body) = terminate(&router, &microvm_id).await;
        assert_eq!(status, 200, "case {case:?}: terminate: got {status} {body}");

        // While the termination is in flight, and again once it is done: one reason.
        for (stage, expected_state) in [("in flight", "TERMINATING"), ("done", "TERMINATED")] {
            if stage == "done" {
                poll_until_microvm_state(&router, case, &microvm_id, expected_state).await;
            }
            let (status, got) = get_microvm(&router, &microvm_id).await;
            assert_eq!(
                status, 200,
                "case {case:?} ({stage}): read: got {status} {got}"
            );
            assert_eq!(
                got["state"].as_str(),
                Some(expected_state),
                "case {case:?} ({stage}): expected state {expected_state}, got {:?}",
                got["state"]
            );
            assert_eq!(
                got["stateReason"].as_str(),
                Some(SUCCESSFUL_TERMINATION),
                "case {case:?} ({stage}): expected stateReason {SUCCESSFUL_TERMINATION:?}, \
                 got {:?}",
                got["stateReason"]
            );
        }
    }
}
