// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for STOP PROTECTION, driven over the wire the service actually speaks —
//! `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rule under test: an instance whose `disableApiStop` attribute is set cannot be stopped
//! through the API. "Indicates whether stop protection is enabled for the instance"
//! (`InstanceAttribute$DisableApiStop`, which a launch asks for through
//! `RunInstancesRequest$DisableApiStop` and `ModifyInstanceAttribute` clears), so:
//!
//! * `StopInstances` naming such an instance is refused HTTP 400 `OperationNotPermitted`,
//!   with the sentence that names the instance and the attribute to modify, and the instance
//!   stays in the state it was in — a `DryRun` probe of that call is refused for the same
//!   reason rather than reporting that the request would have succeeded;
//! * the whole batch is checked before anything moves, so an unprotected instance named
//!   beside a protected one is not stopped either;
//! * once the attribute is cleared the stop is served and reports the state the instance
//!   really held before it.
//!
//! Each row is a distinct input along what the rule generalises over — how the protection was
//! set (at launch or afterwards), where in the batch the protected instance sits, and whether
//! the call is a dry run — so no row is the specification.

use std::time::Duration;

use ec2::state::TRANSITION_DELAY;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, described_instance_state, element, error_code, error_message, indexed};

/// The Region every call of this suite is signed for.
const REGION: &str = "us-west-2";

/// How long a poll loop keeps looking before it gives up: a generous multiple of the span
/// the service moves a launched instance over, so a loaded host is never why a case fails.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll loop waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

fn p(name: &str, value: impl Into<String>) -> (String, String) {
    (name.to_string(), value.into())
}

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One call that must be served, as its body.
async fn ok(router: &MultiServiceRouter, case: &str, action: &str, params: &Params) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Launch one instance with `extra` beside the counts, and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str, extra: Params) -> String {
    let mut params = vec![
        p("ImageId", "ami-0123456789abcdef0"),
        p("MinCount", "1"),
        p("MaxCount", "1"),
    ];
    params.extend(extra);
    let body = ok(router, case, "RunInstances", &params).await;
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances answered no instanceId: {body}"))
}

/// The `disableApiStop` `DescribeInstanceAttribute` reports for one instance.
async fn reported_stop_protection(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> Option<String> {
    let body = ok(
        router,
        case,
        "DescribeInstanceAttribute",
        &vec![
            p("InstanceId", instance_id),
            p("Attribute", "disableApiStop"),
        ],
    )
    .await;
    element(&body, "value")
}

/// Set the instance's `disableApiStop` attribute to `enabled`.
async fn set_stop_protection(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    enabled: bool,
) {
    ok(
        router,
        case,
        "ModifyInstanceAttribute",
        &vec![
            p("InstanceId", instance_id),
            p("DisableApiStop.Value", enabled.to_string()),
        ],
    )
    .await;
}

/// Wait until `instance_id` reports `running`, looking again until it does or until the
/// deadline passes — the CONDITION rather than a fixed wait, so the case does not depend on
/// how fast the host is.
async fn until_running(router: &MultiServiceRouter, case: &str, instance_id: &str) {
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let state = described_instance_state(router, REGION, instance_id).await;
        if state == "running" {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: {instance_id} never reached running (last state {state:?}); the \
             service owes that move {TRANSITION_DELAY:?} after the launch"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The sentence EC2 refuses a stop-protected instance with.
fn refusal_message(instance_id: &str) -> String {
    format!(
        "The instance '{instance_id}' may not be stopped. Modify its 'disableApiStop' \
         instance attribute and try again."
    )
}

/// How the row's protected instance got its protection.
#[derive(Debug, Clone, Copy)]
enum Protection {
    /// `RunInstancesRequest$DisableApiStop` on the launch itself.
    AtLaunch,
    /// `ModifyInstanceAttribute` after the launch.
    Afterwards,
}

/// A protected instance refuses `StopInstances` and stays where it was, however the
/// protection was set, wherever in the batch it sits and whether or not the call is a dry
/// run.
#[tokio::test]
async fn a_stop_protected_instance_refuses_the_stop_and_stays_running() {
    struct Row {
        case: &'static str,
        protection: Protection,
        /// Whether the batch names an unprotected instance BEFORE the protected one.
        with_a_sibling: bool,
        /// Whether the stop is a `DryRun` probe.
        dry_run: bool,
    }

    let rows = [
        Row {
            case: "protection asked for at launch",
            protection: Protection::AtLaunch,
            with_a_sibling: false,
            dry_run: false,
        },
        Row {
            case: "protection set after the launch",
            protection: Protection::Afterwards,
            with_a_sibling: false,
            dry_run: false,
        },
        Row {
            case: "a batch naming an unprotected instance first",
            protection: Protection::AtLaunch,
            with_a_sibling: true,
            dry_run: false,
        },
        Row {
            case: "a dry run of the stop",
            protection: Protection::AtLaunch,
            with_a_sibling: false,
            dry_run: true,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = emulator();
        let protected = match row.protection {
            Protection::AtLaunch => {
                launch(&router, case, vec![p("DisableApiStop", "true")]).await
            }
            Protection::Afterwards => {
                let instance_id = launch(&router, case, Vec::new()).await;
                set_stop_protection(&router, case, &instance_id, true).await;
                instance_id
            }
        };
        let sibling = if row.with_a_sibling {
            Some(launch(&router, case, Vec::new()).await)
        } else {
            None
        };
        // The attribute reads back as the protection the instance really has, whichever way
        // it was set.
        let reported = reported_stop_protection(&router, case, &protected).await;
        assert_eq!(
            reported.as_deref(),
            Some("true"),
            "case {case:?}: expected disableApiStop to read back true, got {reported:?}"
        );
        until_running(&router, case, &protected).await;

        let mut params = Vec::new();
        if let Some(sibling) = sibling.as_deref() {
            params.push(p("InstanceId.1", sibling));
            params.push(p("InstanceId.2", protected.clone()));
        } else {
            params.push(p("InstanceId.1", protected.clone()));
        }
        if row.dry_run {
            params.push(p("DryRun", "true"));
        }
        let (status, body) = call(&router, REGION, "StopInstances", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from StopInstances, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "OperationNotPermitted",
            "case {case:?}: expected OperationNotPermitted, got {body}"
        );
        assert_eq!(
            error_message(&body),
            refusal_message(&protected),
            "case {case:?}: expected the stop-protection sentence, got {body}"
        );

        // Nothing moved: not the protected instance, and not the one named beside it.
        let state = described_instance_state(&router, REGION, &protected).await;
        assert_eq!(
            state, "running",
            "case {case:?}: a refused stop must leave the instance running, got {state:?}"
        );
        if let Some(sibling) = sibling.as_deref() {
            let sibling_state = described_instance_state(&router, REGION, sibling).await;
            assert!(
                sibling_state == "pending" || sibling_state == "running",
                "case {case:?}: a refused batch must not have stopped {sibling}, got \
                 {sibling_state:?}"
            );
        }
    }
}

/// Clearing the attribute is what lets the stop through: the same instance is then stopped,
/// and the call reports the state it really held before it.
#[tokio::test]
async fn clearing_the_attribute_lets_the_stop_through() {
    let case = "the attribute cleared after a refusal";
    let router = emulator();
    let instance_id = launch(&router, case, vec![p("DisableApiStop", "true")]).await;
    until_running(&router, case, &instance_id).await;

    let refused = call(
        &router,
        REGION,
        "StopInstances",
        &indexed("InstanceId", &[&instance_id]),
    )
    .await;
    assert_eq!(
        refused.0, 400,
        "case {case:?}: the protected stop must be refused, got {refused:?}"
    );

    set_stop_protection(&router, case, &instance_id, false).await;
    let reported = reported_stop_protection(&router, case, &instance_id).await;
    assert_eq!(
        reported.as_deref(),
        Some("false"),
        "case {case:?}: expected disableApiStop to read back false, got {reported:?}"
    );

    let body = ok(
        &router,
        case,
        "StopInstances",
        &indexed("InstanceId", &[&instance_id]),
    )
    .await;
    let document = super::wire::document(case, &body);
    let change = document
        .descendants()
        .find(|node| node.is_element() && super::wire::child(*node, "currentState").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no state change in {body}"));
    let state_name = |member: &str| {
        super::wire::child(change, member)
            .and_then(|state| super::wire::child_text(state, "name"))
            .unwrap_or_default()
    };
    assert_eq!(
        state_name("previousState"),
        "running",
        "case {case:?}: expected the previous state the instance really held, got {body}"
    );
    assert_eq!(
        state_name("currentState"),
        "stopping",
        "case {case:?}: expected the instance to be stopping, got {body}"
    );
}

/// The control: an instance nothing protected is stopped, so the refusal above is the
/// attribute's doing rather than a stop this service never serves.
#[tokio::test]
async fn an_unprotected_instance_is_stopped() {
    let case = "an instance with no stop protection";
    let router = emulator();
    let instance_id = launch(&router, case, Vec::new()).await;
    let reported = reported_stop_protection(&router, case, &instance_id).await;
    assert_eq!(
        reported.as_deref(),
        Some("false"),
        "case {case:?}: a launch that asked for no protection has none, got {reported:?}"
    );
    ok(
        &router,
        case,
        "StopInstances",
        &indexed("InstanceId", &[&instance_id]),
    )
    .await;
    let state = described_instance_state(&router, REGION, &instance_id).await;
    assert_eq!(
        state, "stopping",
        "case {case:?}: expected the instance to be stopping, got {state:?}"
    );
}
