// SPDX-License-Identifier: CC0-1.0

//! `DeregisterTargetFromMaintenanceWindow` as a REMOVAL rather than a lookup: the call asks
//! for a state — the target it names is not registered with the window — and a window that
//! already holds none of that target is already in it.
//!
//! The RULE under test: a deregistration whose `WindowTargetId` an existing window holds no
//! target for is served 200 echoing the `WindowId` and `WindowTargetId` the request carried,
//! writes nothing and resurrects nothing, so the call converges however many times it is made;
//! `DoesNotExistException` is the refusal of the RESOURCE the request addresses, the
//! maintenance window, which is reached through the endpoint of ONE Region.
//!
//! Every call goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router — and every target
//! a row removes was registered by a `RegisterTargetWithMaintenanceWindow` of this suite, read
//! back through `DescribeMaintenanceWindowTargets`, so nothing here can pass on a response
//! that is not computed from persisted state.
//!
//! The table's rows differ along what the rule generalises over: which target id the removal
//! names (one a register minted and a removal already took, one of a SIBLING window, one
//! nothing ever minted), how many times the same removal is repeated, and what the request
//! asks of the `Safe` check — none of which changes the answer, because the state it asks for
//! is already reached.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, error_message, router, REGION};

/// A `WindowId` of the form the modelled `MaintenanceWindowId` pattern requires
/// (`^mw-[0-9a-f]{17}$`) that no create has minted.
const UNKNOWN_WINDOW_ID: &str = "mw-0123456789abcdef0";

/// A `WindowTargetId` of the 8-4-4-4-12 hexadecimal form the modelled
/// `MaintenanceWindowTargetId` pattern requires that no register has minted.
const UNKNOWN_TARGET_ID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

/// How many times one row repeats the removal it is about: more than twice, because the rule
/// is about the STATE the call asks for and not about the second call.
const REPEATS: usize = 4;

/// Create a maintenance window and answer the id it minted.
async fn window(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, response) = call_in_region(
        router,
        REGION,
        "CreateMaintenanceWindow",
        &json!({
            "Name": name,
            "Schedule": "rate(1 day)",
            "Duration": 2,
            "Cutoff": 1,
            "AllowUnassociatedTargets": false,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the window must be created first, got {status} {response}"
    );
    response["WindowId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create reports a WindowId, got {response}"))
        .to_string()
}

/// Register one target with `window_id` and answer the `WindowTargetId` it minted.
async fn target(router: &MultiServiceRouter, case: &str, window_id: &str) -> String {
    let (status, response) = call_in_region(
        router,
        REGION,
        "RegisterTargetWithMaintenanceWindow",
        &json!({
            "WindowId": window_id,
            "ResourceType": "INSTANCE",
            "Targets": [{ "Key": "tag:Name", "Values": ["probe"] }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the target must be registered first, got {status} {response}"
    );
    response["WindowTargetId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a register reports a WindowTargetId, got {response}")
        })
        .to_string()
}

/// The `WindowTargetId`s `window_id` holds, in the order the describe reports them.
async fn held(router: &MultiServiceRouter, case: &str, window_id: &str) -> Vec<String> {
    let (status, response) = call_in_region(
        router,
        REGION,
        "DescribeMaintenanceWindowTargets",
        &json!({ "WindowId": window_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe must be served, got {status} {response}"
    );
    response["Targets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|target| target["WindowTargetId"].as_str().map(str::to_string))
        .collect()
}

/// `DeregisterTargetFromMaintenanceWindow` with `body`.
async fn deregister(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call_in_region(router, REGION, "DeregisterTargetFromMaintenanceWindow", body).await
}

/// What the `Safe` member of a row's request carries.
#[derive(Debug, Clone, Copy)]
enum Safe {
    /// The request names it not at all, which is the safe reading.
    Unstated,
    /// The request asks for the reference check explicitly.
    Checked,
    /// The request asks for the removal to go through regardless.
    Unchecked,
}

impl Safe {
    /// This choice as a request spells it.
    fn stated(self) -> Option<bool> {
        match self {
            Self::Unstated => None,
            Self::Checked => Some(true),
            Self::Unchecked => Some(false),
        }
    }
}

/// Which target id a row's removal names.
#[derive(Debug, Clone, Copy)]
enum Named {
    /// The one this window's register minted — the removal that really removes something.
    Registered,
    /// The one a register of a SIBLING window minted, which this window holds none of.
    SiblingsOwn,
    /// One no register of any window minted.
    NeverMinted,
}

/// One row of the table: whose target id the removal names, and what it asks of the check.
struct Row {
    case: &'static str,
    named: Named,
    safe: Safe,
}

/// RULE: a removal of a target an existing window holds none of is the state the call asks
/// for, so it is served 200 echoing the request's ids and writes nothing — however often it is
/// repeated, whichever well-formed id it names, and whatever it asks of the `Safe` check.
#[tokio::test]
async fn a_removal_of_a_target_that_is_already_gone_is_served_and_writes_nothing() {
    let rows = [
        Row {
            case: "the target this window's register minted",
            named: Named::Registered,
            safe: Safe::Unstated,
        },
        Row {
            case: "the same, with the reference check asked for",
            named: Named::Registered,
            safe: Safe::Checked,
        },
        Row {
            case: "the same, with the check waived",
            named: Named::Registered,
            safe: Safe::Unchecked,
        },
        Row {
            case: "a target id a sibling window holds",
            named: Named::SiblingsOwn,
            safe: Safe::Unstated,
        },
        Row {
            case: "a target id nothing ever minted",
            named: Named::NeverMinted,
            safe: Safe::Checked,
        },
    ];

    for row in rows {
        let Row { case, named, safe } = row;
        let router = router();
        let window_id = window(&router, case, "removal-window").await;
        let registered = target(&router, case, &window_id).await;
        // A target of the window every row keeps, so each row also shows the removal touches
        // no other target of the same window.
        let kept = target(&router, case, &window_id).await;
        let sibling = window(&router, case, "sibling-window").await;
        let siblings_own = target(&router, case, &sibling).await;

        let named_id = match named {
            Named::Registered => registered.clone(),
            Named::SiblingsOwn => siblings_own.clone(),
            Named::NeverMinted => UNKNOWN_TARGET_ID.to_string(),
        };
        let mut request = json!({ "WindowId": &window_id, "WindowTargetId": &named_id });
        if let Some(stated) = safe.stated() {
            request["Safe"] = json!(stated);
        }

        // The removal is repeated: the FIRST call of the registered row removes something and
        // every later one asks for a state already reached, while every call of the other
        // rows asks for one that was reached before this suite started.
        for attempt in 1..=REPEATS {
            let (status, response) = deregister(&router, &request).await;
            assert_eq!(
                (
                    status,
                    response["WindowId"].as_str(),
                    response["WindowTargetId"].as_str(),
                ),
                (200, Some(window_id.as_str()), Some(named_id.as_str())),
                "case {case:?}: removal {attempt} of {REPEATS} must echo the ids it was asked \
                 about, got {status} {response}"
            );
        }

        // What the window HOLDS: the target no row removed, and nothing else — the repeated
        // removals wrote nothing, and nothing resurrected the one the first call took.
        let remaining = held(&router, case, &window_id).await;
        let expected = match named {
            Named::Registered => vec![kept.clone()],
            _ => vec![registered.clone(), kept.clone()],
        };
        assert_eq!(
            remaining, expected,
            "case {case:?}: expected the window to hold {expected:?}, got {remaining:?}"
        );
        // And the sibling window is untouched by a removal addressed to this one.
        let siblings = held(&router, case, &sibling).await;
        assert_eq!(
            siblings,
            vec![siblings_own.clone()],
            "case {case:?}: expected the sibling window to keep its own target, got {siblings:?}"
        );
    }
}

/// RULE, the other half: the WINDOW is the resource the request addresses, so a `WindowId`
/// this endpoint holds no window for is refused `DoesNotExistException` naming that window —
/// whether or not the target id beside it is one some window holds.
#[tokio::test]
async fn a_removal_addressed_to_a_window_this_endpoint_holds_none_of_is_refused() {
    let case = "the refused removal";
    let router = router();
    let window_id = window(&router, case, "live-window").await;
    let registered = target(&router, case, &window_id).await;
    // A window of ANOTHER Region's endpoint, which this endpoint holds no window for either.
    let elsewhere = {
        let (status, response) = call_in_region(
            &router,
            "eu-central-1",
            "CreateMaintenanceWindow",
            &json!({
                "Name": "other-region-window",
                "Schedule": "rate(1 day)",
                "Duration": 2,
                "Cutoff": 1,
                "AllowUnassociatedTargets": false,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the other Region's window must be created, got {status} {response}"
        );
        response["WindowId"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };

    // (case, the window the removal names, the target it names)
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "a window id nothing minted, naming a target this Region holds",
            UNKNOWN_WINDOW_ID,
            registered.as_str(),
        ),
        (
            "a window id nothing minted, naming a target nothing minted",
            UNKNOWN_WINDOW_ID,
            UNKNOWN_TARGET_ID,
        ),
        (
            "a window of another Region's endpoint",
            elsewhere.as_str(),
            registered.as_str(),
        ),
    ];

    for (why, names_window, names_target) in rows {
        let (status, response) = deregister(
            &router,
            &json!({ "WindowId": names_window, "WindowTargetId": names_target }),
        )
        .await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "DoesNotExistException"),
            "case {why:?}: expected the removal to be refused, got {status} {response}"
        );
        assert_eq!(
            error_message(&response),
            format!("Maintenance window {names_window} does not exist"),
            "case {why:?}: expected the refusal to name the window that does not exist, got \
             {response}"
        );
    }

    // The refusals wrote nothing: the live window still holds the target it was registered
    // with, and the removal that names both of its own ids still works.
    assert_eq!(
        held(&router, case, &window_id).await,
        vec![registered.clone()],
        "case {case:?}: expected the refused removals to have written nothing"
    );
    let (status, response) = deregister(
        &router,
        &json!({ "WindowId": &window_id, "WindowTargetId": &registered }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the removal of its own target must be served, got {status} {response}"
    );
    assert!(
        held(&router, case, &window_id).await.is_empty(),
        "case {case:?}: expected the window to hold no target once its own was removed"
    );
}
