// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WORDING of the refusal an ARN-bearing member that is not an ARN
//! of the modelled shape is answered with.
//!
//! THE RULE UNDER TEST: every member of this API that carries an ARN targets the `Arn`
//! shape — a string of length 1..255 matching `^[A-Za-z0-9:.\/_-]*$`
//! (`smithy_query(query="shape", shape="Arn", model_path="services/service-com-amazonaws-route53recoverycluster-togglecustomerapi/smithy-model/com-amazonaws-route53recoverycluster-togglecustomerapi.json")`)
//! — and a request whose ARN member is absent or outside that shape is refused with
//! `ValidationException` (HTTP 400) and ONE fixed sentence that echoes nothing of the
//! request: not the value, not the member name, and not the pattern. A value INSIDE the
//! shape reaches the operation's own answer, even when it names nothing.
//!
//! The rows differ along the form of the violation: each class of character the pattern
//! excludes (the recorded corruption, punctuation, a wildcard, a non-ASCII letter), each
//! published length bound and one past it, and an absent member — and along the member that
//! carries the ARN, so every ARN-bearing member of every operation is covered.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::common::{
    call, create_cluster, create_control_panel, create_routing_control, emulator, error_code,
    error_message, Control, Panel,
};

/// How this API words a request whose ARN-bearing member is absent or is not an ARN of the
/// modelled shape. The WHOLE string is fixed text.
///
/// Spelled out here independently of the emulator's own constant, so a change to that
/// constant has to agree with this sentence rather than redefine it.
const INVALID_RESOURCE_ARN: &str = "Missing or invalid resource arn";

/// The published bounds of the `Arn` shape's `@length` trait.
const ARN_MIN_LENGTH: usize = 1;
const ARN_MAX_LENGTH: usize = 255;

/// What a row expects back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// This API's fixed refusal of a value that is not an ARN of the modelled shape.
    InvalidArn,
    /// The value is inside the modelled shape, so the operation answers for itself: the
    /// resource it names does not exist.
    NotFound,
    /// The value is inside the modelled shape and names a real resource.
    Served,
}

/// One control panel holding one routing control, `Off`.
async fn fixture() -> (MultiServiceRouter, Panel, Control) {
    let router = emulator();
    let cluster = create_cluster(&router, "arn-refusal").await;
    let mut panel = create_control_panel(&router, &cluster, "arn-refusal-panel").await;
    let control = create_routing_control(&router, &panel, "control-0").await;
    panel.controls.push(control.clone());
    (router, panel, control)
}

/// Assert the answer to one request matches `expect`.
async fn assert_answer(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
    expect: Expect,
) {
    let (status, body) = call(router, operation, request).await;
    match expect {
        Expect::InvalidArn => {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "ValidationException"),
                "case {case:?}: expected 400 ValidationException, got {status} with {body}"
            );
            let actual = error_message(&body);
            assert_eq!(
                actual, INVALID_RESOURCE_ARN,
                "case {case:?}: expected message {INVALID_RESOURCE_ARN:?}, got {actual:?}"
            );
        }
        Expect::NotFound => {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?}: a well-formed ARN naming nothing is the operation's own \
                 refusal, got {status} with {body}"
            );
            let actual = error_message(&body);
            assert_ne!(
                actual, INVALID_RESOURCE_ARN,
                "case {case:?}: a well-formed ARN must not be refused as malformed, got \
                 {actual:?}"
            );
        }
        Expect::Served => {
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} with {body}"
            );
        }
    }
}

/// Values a `RoutingControlArn` row sends, built from the real ARN `arn`: what the value
/// is, and what the answer must be.
fn arn_values(arn: &str) -> Vec<(String, String, Expect)> {
    // Every character the `Arn` pattern excludes: its character class admits letters,
    // digits, `:`, `.`, `/`, `_` and `-` only, so a space, punctuation, a wildcard and any
    // non-ASCII letter are all outside it.
    let corrupted = format!("{}!", arn.replacen(':', " ", 1));
    vec![
        (
            // The corruption the recorded call made: the first `:` replaced by a space and a
            // `!` appended.
            "a space and an exclamation mark".to_string(),
            corrupted,
            Expect::InvalidArn,
        ),
        (
            "a wildcard character".to_string(),
            format!("{arn}*"),
            Expect::InvalidArn,
        ),
        (
            "a percent sign".to_string(),
            format!("{arn}%20"),
            Expect::InvalidArn,
        ),
        (
            "a comma".to_string(),
            format!("{arn},{arn}"),
            Expect::InvalidArn,
        ),
        (
            "a non-ASCII letter".to_string(),
            format!("{arn}é"),
            Expect::InvalidArn,
        ),
        (
            "an empty value, one below the minimum length".to_string(),
            String::new(),
            Expect::InvalidArn,
        ),
        (
            "one character past the maximum length".to_string(),
            "a".repeat(ARN_MAX_LENGTH + 1),
            Expect::InvalidArn,
        ),
        (
            "the minimum length, inside the pattern".to_string(),
            "a".repeat(ARN_MIN_LENGTH),
            Expect::NotFound,
        ),
        (
            "the maximum length, inside the pattern".to_string(),
            "a".repeat(ARN_MAX_LENGTH),
            Expect::NotFound,
        ),
        (
            "every character the pattern admits".to_string(),
            "aZ0:./_-".to_string(),
            Expect::NotFound,
        ),
        (
            "the routing control's own ARN".to_string(),
            arn.to_string(),
            Expect::Served,
        ),
    ]
}

/// RULE: a `RoutingControlArn` outside the modelled `Arn` shape is this API's fixed
/// refusal, and one inside it reaches the operation's own answer — on both operations that
/// take one directly, and inside a batch entry.
#[tokio::test]
async fn a_routing_control_arn_outside_the_modelled_shape_is_refused_with_the_fixed_sentence() {
    let (router, _panel, control) = fixture().await;

    for (label, value, expect) in arn_values(&control.arn) {
        // `Served` moves the switch, so the read is asked for first and the writes after:
        // every row's answer depends on its own value alone.
        let case = format!("GetRoutingControlState with {label}");
        assert_answer(
            &router,
            &case,
            "GetRoutingControlState",
            &json!({ "RoutingControlArn": value }),
            expect,
        )
        .await;

        let case = format!("UpdateRoutingControlState with {label}");
        assert_answer(
            &router,
            &case,
            "UpdateRoutingControlState",
            &json!({ "RoutingControlArn": value, "RoutingControlState": "On" }),
            expect,
        )
        .await;

        let case = format!("UpdateRoutingControlStates with {label}");
        assert_answer(
            &router,
            &case,
            "UpdateRoutingControlStates",
            &json!({
                "UpdateRoutingControlStateEntries": [
                    { "RoutingControlArn": value, "RoutingControlState": "On" },
                ],
            }),
            expect,
        )
        .await;
    }
}

/// RULE: the same refusal covers every other ARN-bearing member — the `ControlPanelArn` a
/// listing is filtered by, and an element of the `SafetyRulesToOverride` list either update
/// may carry — and an absent required ARN, which the sentence's own wording covers.
#[tokio::test]
async fn every_arn_bearing_member_is_refused_with_the_same_sentence() {
    let (router, panel, control) = fixture().await;
    let malformed = format!("{}!", control.arn.replacen(':', " ", 1));

    // (case, operation, the request, what it must answer)
    let rows: Vec<(&str, &str, Value, Expect)> = vec![
        (
            "a malformed ControlPanelArn filter",
            "ListRoutingControls",
            json!({ "ControlPanelArn": malformed }),
            Expect::InvalidArn,
        ),
        (
            "the real ControlPanelArn filter",
            "ListRoutingControls",
            json!({ "ControlPanelArn": panel.arn }),
            Expect::Served,
        ),
        (
            "a malformed ARN in SafetyRulesToOverride",
            "UpdateRoutingControlState",
            json!({
                "RoutingControlArn": control.arn,
                "RoutingControlState": "On",
                "SafetyRulesToOverride": [malformed],
            }),
            Expect::InvalidArn,
        ),
        (
            "a malformed ARN in the batch's SafetyRulesToOverride",
            "UpdateRoutingControlStates",
            json!({
                "UpdateRoutingControlStateEntries": [
                    { "RoutingControlArn": control.arn, "RoutingControlState": "On" },
                ],
                "SafetyRulesToOverride": [malformed],
            }),
            Expect::InvalidArn,
        ),
        (
            "an absent RoutingControlArn",
            "GetRoutingControlState",
            json!({}),
            Expect::InvalidArn,
        ),
        (
            "an absent RoutingControlArn in a batch entry",
            "UpdateRoutingControlStates",
            json!({
                "UpdateRoutingControlStateEntries": [{ "RoutingControlState": "On" }],
            }),
            Expect::InvalidArn,
        ),
    ];

    for (case, operation, request, expect) in &rows {
        assert_answer(&router, case, operation, request, *expect).await;
    }
}

/// RULE: only the ARN members are reworded. A member this model constrains that carries no
/// ARN keeps the framework's own constraint report, so the fixed ARN sentence is not this
/// service's answer to every refused value.
#[tokio::test]
async fn a_refused_member_that_carries_no_arn_keeps_its_own_wording() {
    let (router, _panel, control) = fixture().await;

    // (case, operation, the request) — each refuses a non-ARN member: a `NextToken` outside
    // `^[\S]*$`, a `MaxResults` below its `@range` minimum of 1, and a routing control state
    // outside the modelled enum.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "a page token carrying whitespace",
            "ListRoutingControls",
            json!({ "NextToken": "not a token" }),
        ),
        (
            "a page size below the modelled minimum",
            "ListRoutingControls",
            json!({ "MaxResults": 0 }),
        ),
        (
            "a routing control state outside the modelled enum",
            "UpdateRoutingControlStates",
            json!({
                "UpdateRoutingControlStateEntries": [
                    { "RoutingControlArn": control.arn, "RoutingControlState": "ON" },
                ],
            }),
        ),
    ];

    for (case, operation, request) in &rows {
        let (status, body) = call(&router, operation, request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} with {body}"
        );
        let actual = error_message(&body);
        assert_ne!(
            actual, INVALID_RESOURCE_ARN,
            "case {case:?}: a member carrying no ARN must keep its own wording, got {actual:?}"
        );
        assert!(
            !actual.is_empty(),
            "case {case:?}: expected a message, got {actual:?}"
        );
    }
}
