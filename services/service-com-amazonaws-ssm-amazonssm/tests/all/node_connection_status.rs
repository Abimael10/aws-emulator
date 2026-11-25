// SPDX-License-Identifier: CC0-1.0

//! The Session Manager connection status of a managed node, end to end.
//!
//! RULE under test: `GetConnectionStatus` "retrieves the Session Manager connection status
//! for a managed node to determine whether it is running and ready to receive Session
//! Manager connections", so it reports `connected` exactly for a target THIS endpoint holds
//! as a managed node whose agent is responding — the same fact that decides whether
//! `StartSession` opens a session to it — and `notconnected` for every other target, with
//! the target echoed as the request named it. Nothing about the SESSIONS of the target is
//! read: a connected node is ready to RECEIVE a connection whether or not one is open.
//!
//! What the report is asked about is "the managed node ID"
//! (`GetConnectionStatusRequest$Target`), so a value that is no node id at all names nothing
//! a status could be about and is REFUSED (400 `ValidationException`, "Invalid target ID
//! format") rather than reported `notconnected` — which would read as a connectivity answer
//! about a node that cannot exist.
//!
//! The rows differ along what the rule generalises over: which kind of target is named (a
//! registered node, an instance that never registered, one whose role withholds the
//! registration call, an instance id nothing was launched as, a hybrid-activation node id, a
//! value that is no node id at all, and a target at the published maximum length), and which
//! Region's endpoint is asked — a node is held per Region, so the same id is a node at one
//! endpoint and no node at another.
//!
//! Every call goes through the wire a client uses, and the managed node the connected rows
//! are about is one an Amazon EC2 launch registered — the registration this service derives
//! from the three components that own its evidence ([`crate::ec2_managed_node_registration`]),
//! whose helpers this suite drives rather than keeping a second copy of them.
//!
//! Nothing here waits a fixed time: the registration is a stored due instant, so a case
//! LOOKS AGAIN until the condition holds or the suite's generous deadline passes.

use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_node_registration::{
    create_profile, emulator, image_id, launch, node_when_registered, Grant, Profile,
    AL2023_PARAMETER, DEADLINE, POLL_PAUSE,
};
use crate::parameter_store::{call_in_region, error_code, error_message, REGION};

/// Another Region's endpoint, for the rows that ask it about a node this Region holds.
const OTHER_REGION: &str = "eu-central-1";

/// The status a reachable node is reported in (the modelled `ConnectionStatus` enum,
/// `smithy_query(query="operation", operation="GetConnectionStatus")`: `connected |
/// notconnected`).
const CONNECTED: &str = "connected";

/// The status every other target is reported in (same enum).
const NOTCONNECTED: &str = "notconnected";

/// The published maximum of `SessionTarget`, which `GetConnectionStatus$Target` is
/// (`smithy_query(query="operation", operation="GetConnectionStatus")`: `len 1..400`).
const MAX_TARGET_CHARS: usize = 400;

/// The published minimum of the same member.
const MIN_TARGET_CHARS: usize = 1;

/// How the report refuses a target that is not a managed-node ID at all — fixed text, with
/// nothing of the value echoed (`state::sessions::INVALID_TARGET_ID_FORMAT`), written out here
/// so the suite pins the sentence rather than echoing whatever the service holds.
const INVALID_TARGET_ID_FORMAT: &str = "Invalid target ID format";

/// The instance profile a row's instance runs as when it is to become a managed node: its
/// role carries the AWS-managed policy a client attaches so the agent may register.
fn registering_profile() -> Profile {
    Profile {
        name: "ssm-core",
        trusts: "ec2.amazonaws.com",
        grant: Grant::AwsManaged,
    }
}

/// An instance profile whose role is granted nothing, so the instance running as it never
/// becomes a managed node.
fn silent_profile() -> Profile {
    Profile {
        name: "no-grant",
        trusts: "ec2.amazonaws.com",
        grant: Grant::None,
    }
}

/// Ask `region`'s endpoint for `target`'s connection status, as the status and body it
/// answered.
async fn connection_status(
    router: &MultiServiceRouter,
    region: &str,
    target: &str,
) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "GetConnectionStatus",
        &json!({ "Target": target }),
    )
    .await
}

/// The `Status` `region`'s endpoint reports for `target`, asserting the call was served and
/// that the report echoes the target it was asked about.
async fn reported_status(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    target: &str,
) -> String {
    let (status, body) = connection_status(router, region, target).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetConnectionStatus must be served, got {status} with {body}"
    );
    let echoed = body["Target"].as_str().unwrap_or_default();
    assert_eq!(
        echoed, target,
        "case {case:?}: expected the report to echo the target {target:?}, got {echoed:?}"
    );
    let reported = body["Status"].as_str().unwrap_or_default().to_string();
    assert!(
        reported == CONNECTED || reported == NOTCONNECTED,
        "case {case:?}: expected a modelled ConnectionStatus, got {reported:?} in {body}"
    );
    reported
}

/// Look again until `region`'s endpoint reports `target` connected, or the deadline passes.
async fn connected_when_reachable(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    target: &str,
) {
    let until = Instant::now() + DEADLINE;
    loop {
        let reported = reported_status(router, case, region, target).await;
        if reported == CONNECTED {
            return;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected {target} to be reported {CONNECTED}, and the endpoint \
             kept reporting {reported}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// One row of the table: the endpoint asked, the target it is asked about, whether that
/// target is spelled as a managed-node ID at all, and the status it must be reported in when
/// it is.
#[derive(Debug, Clone)]
struct Case {
    case: &'static str,
    region: &'static str,
    target: String,
    /// Whether the target is a managed-node ID form, which is what the report is about: a
    /// value that is not is refused instead of being reported on.
    node_id: bool,
    connected: bool,
}

/// RULE: a target is `connected` exactly when the endpoint asked holds it as a managed node
/// whose agent is responding, over the kinds of target a caller names and both endpoints.
#[tokio::test]
async fn a_target_is_connected_exactly_when_this_endpoint_holds_it_as_a_responding_node() {
    let router = emulator();
    let registering = registering_profile();
    let silent = silent_profile();
    create_profile(&router, &registering).await;
    create_profile(&router, &silent).await;
    let image = image_id(&router, AL2023_PARAMETER).await;

    // The instances that must NOT register are launched first, so that by the time the
    // registration of the last one has been observed every earlier instance's own span has
    // passed too — which is what makes "it is not connected" an answer rather than a race.
    let no_profile = launch(&router, "no instance profile", REGION, &image, None).await;
    let no_grant = launch(
        &router,
        "a role granted nothing",
        REGION,
        &image,
        Some(silent.name),
    )
    .await;
    let registered = launch(
        &router,
        "a registered node",
        REGION,
        &image,
        Some(registering.name),
    )
    .await;
    let elsewhere = launch(
        &router,
        "a node of another Region",
        OTHER_REGION,
        &image,
        Some(registering.name),
    )
    .await;

    // Both registrations are a stored due instant: look again until this service reports
    // each instance as a managed node of its own Region.
    node_when_registered(&router, "a registered node", REGION, &registered).await;
    node_when_registered(
        &router,
        "a node of another Region",
        OTHER_REGION,
        &elsewhere,
    )
    .await;

    let rows: Vec<Case> = vec![
        Case {
            case: "a registered node, at its own endpoint",
            region: REGION,
            target: registered.clone(),
            node_id: true,
            connected: true,
        },
        Case {
            case: "a node of another Region, at its own endpoint",
            region: OTHER_REGION,
            target: elsewhere.clone(),
            node_id: true,
            connected: true,
        },
        Case {
            case: "a registered node, at another Region's endpoint",
            region: OTHER_REGION,
            target: registered.clone(),
            node_id: true,
            connected: false,
        },
        Case {
            case: "another Region's node, at this endpoint",
            region: REGION,
            target: elsewhere.clone(),
            node_id: true,
            connected: false,
        },
        Case {
            case: "an instance launched with no instance profile",
            region: REGION,
            target: no_profile.clone(),
            node_id: true,
            connected: false,
        },
        Case {
            case: "an instance whose role is granted nothing",
            region: REGION,
            target: no_grant.clone(),
            node_id: true,
            connected: false,
        },
        Case {
            case: "an instance id nothing was launched as",
            region: REGION,
            target: "i-00000000000000000".to_string(),
            node_id: true,
            connected: false,
        },
        Case {
            case: "a hybrid-activation node id nothing registered",
            region: REGION,
            target: "mi-0123456789abcdef0".to_string(),
            node_id: true,
            connected: false,
        },
        Case {
            case: "the shorter EC2 instance id width",
            region: REGION,
            target: "i-0abcdef0".to_string(),
            node_id: true,
            connected: false,
        },
        Case {
            case: "a value that is no node id at all",
            region: REGION,
            target: "not-a-managed-node".to_string(),
            node_id: false,
            connected: false,
        },
        Case {
            case: "a target at the published maximum length",
            region: REGION,
            target: "t".repeat(MAX_TARGET_CHARS),
            node_id: false,
            connected: false,
        },
        Case {
            case: "a target at the published minimum length",
            region: REGION,
            target: "t".repeat(MIN_TARGET_CHARS),
            node_id: false,
            connected: false,
        },
        Case {
            case: "an instance id one character short of either width",
            region: REGION,
            target: "i-0abcdef".to_string(),
            node_id: false,
            connected: false,
        },
    ];

    for row in &rows {
        if row.node_id {
            let expected = if row.connected { CONNECTED } else { NOTCONNECTED };
            let reported = reported_status(&router, row.case, row.region, &row.target).await;
            assert_eq!(
                reported, expected,
                "case {:?}: expected {expected}, got {reported}",
                row.case
            );
        } else {
            // "The managed node ID": a target that is no node id names nothing to report on.
            let (status, body) = connection_status(&router, row.region, &row.target).await;
            assert_eq!(
                status, 400,
                "case {:?}: a target that is no node id is refused, got {status} with {body}",
                row.case
            );
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {:?}: expected ValidationException, got {body}",
                row.case
            );
            assert_eq!(
                error_message(&body),
                INVALID_TARGET_ID_FORMAT,
                "case {:?}: expected {INVALID_TARGET_ID_FORMAT:?}, got {body}",
                row.case
            );
        }

        // The report and the start cannot disagree about one target: a target the report
        // calls connected is one a session opens to, and every other target is
        // `TargetNotConnected`.
        let (status, body) = call_in_region(
            &router,
            row.region,
            "StartSession",
            &json!({ "Target": row.target }),
        )
        .await;
        if row.connected {
            assert_eq!(
                status, 200,
                "case {:?}: a connected target starts a session, got {status} with {body}",
                row.case
            );
        } else {
            assert_eq!(
                status, 400,
                "case {:?}: a target that is not connected is refused, got {status} with \
                 {body}",
                row.case
            );
            let code = error_code(&body);
            assert_eq!(
                code, "TargetNotConnected",
                "case {:?}: expected TargetNotConnected, got {code} in {body}",
                row.case
            );
        }
    }
}

/// RULE: the status is about the NODE, so it is the same before a session is opened to it,
/// while one is open and after it has ended — and reporting it opens none.
#[tokio::test]
async fn the_status_of_a_node_does_not_depend_on_its_sessions() {
    let router = emulator();
    let profile = registering_profile();
    create_profile(&router, &profile).await;
    let image = image_id(&router, AL2023_PARAMETER).await;
    let case = "a registered node asked about around a session";
    let instance = launch(&router, case, REGION, &image, Some(profile.name)).await;
    node_when_registered(&router, case, REGION, &instance).await;
    connected_when_reachable(&router, case, REGION, &instance).await;

    /// The sessions `state` holds at this endpoint.
    async fn sessions(router: &MultiServiceRouter, state: &str) -> Vec<Value> {
        let (status, body) =
            call_in_region(router, REGION, "DescribeSessions", &json!({ "State": state })).await;
        assert_eq!(
            status, 200,
            "DescribeSessions({state}) must answer, got {status} with {body}"
        );
        body["Sessions"].as_array().cloned().unwrap_or_default()
    }

    // A read changes nothing a later caller sees: two reports of a node nobody has opened a
    // session to leave the endpoint holding no session at all.
    for _ in 0..2 {
        let reported = reported_status(&router, case, REGION, &instance).await;
        assert_eq!(
            reported, CONNECTED,
            "case {case:?}: expected {CONNECTED} before any session, got {reported}"
        );
    }
    for state in ["Active", "History"] {
        let held = sessions(&router, state).await;
        assert!(
            held.is_empty(),
            "case {case:?}: reporting a connection opens no session, got {state} {held:?}"
        );
    }

    let (status, started) = call_in_region(
        &router,
        REGION,
        "StartSession",
        &json!({ "Target": instance }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: StartSession must be served, got {status} with {started}"
    );
    let session_id = started["SessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the start reports a session id, got {started}"))
        .to_string();
    let reported = reported_status(&router, case, REGION, &instance).await;
    assert_eq!(
        reported, CONNECTED,
        "case {case:?}: expected {CONNECTED} while a session is open, got {reported}"
    );

    let (status, ended) = call_in_region(
        &router,
        REGION,
        "TerminateSession",
        &json!({ "SessionId": session_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: TerminateSession must be served, got {status} with {ended}"
    );
    let reported = reported_status(&router, case, REGION, &instance).await;
    assert_eq!(
        reported, CONNECTED,
        "case {case:?}: expected {CONNECTED} after the session ended, got {reported}"
    );
}

/// The published bounds of the target a report may be asked about, and the node-ID form it has
/// to take inside them: a value outside the modelled `@length` is refused by the boundary
/// clause that names the bound, a value inside it that is no node id is refused by the
/// operation's own sentence, and only a node id is reported on at all.
#[tokio::test]
async fn a_target_outside_the_published_length_or_the_node_id_form_is_refused() {
    let router = emulator();

    /// What a row expects of the answer.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Expected {
        /// A status is reported for the target.
        Reported,
        /// The operation's own sentence: the value is no managed-node ID.
        NotANodeId,
        /// The boundary clause about the member's modelled length.
        OutsideTheLength,
    }

    // (case, the target the request carries, what the endpoint answers)
    let rows: Vec<(&str, String, Expected)> = vec![
        (
            "a node id nothing registered",
            "i-0abcdef0123456789".to_string(),
            Expected::Reported,
        ),
        (
            "one character at the published minimum",
            "t".repeat(MIN_TARGET_CHARS),
            Expected::NotANodeId,
        ),
        (
            "a target at the published maximum",
            "t".repeat(MAX_TARGET_CHARS),
            Expected::NotANodeId,
        ),
        ("below the minimum", String::new(), Expected::OutsideTheLength),
        (
            "past the maximum",
            "t".repeat(MAX_TARGET_CHARS + 1),
            Expected::OutsideTheLength,
        ),
        (
            "far past the maximum",
            "t".repeat(MAX_TARGET_CHARS * 3),
            Expected::OutsideTheLength,
        ),
    ];

    for (case, target, expected) in &rows {
        let (status, body) = connection_status(&router, REGION, target).await;
        if *expected == Expected::Reported {
            assert_eq!(
                status, 200,
                "case {case:?}: a node id is reported on, got {status} with {body}"
            );
            let reported = body["Status"].as_str().unwrap_or_default();
            assert_eq!(
                reported, NOTCONNECTED,
                "case {case:?}: expected {NOTCONNECTED} for a target no node answers for, got \
                 {reported} in {body}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: this target is refused, got {status} with {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "ValidationException",
            "case {case:?}: expected ValidationException, got {code} in {body}"
        );
        let message = error_message(&body);
        match expected {
            Expected::NotANodeId => assert_eq!(
                message, INVALID_TARGET_ID_FORMAT,
                "case {case:?}: expected {INVALID_TARGET_ID_FORMAT:?}, got {message:?}"
            ),
            _ => assert!(
                message.contains("'target'") && message.contains("length"),
                "case {case:?}: expected the clause naming the member's length bound, got \
                 {message:?}"
            ),
        }
    }
}
