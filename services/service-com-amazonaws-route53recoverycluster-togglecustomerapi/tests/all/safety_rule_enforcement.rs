// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the safety rules the Route 53 ARC cluster (data-plane) update
//! operations are gated by: `UpdateRoutingControlState` and
//! `UpdateRoutingControlStates` against assertion rules, gating rules and
//! `SafetyRulesToOverride`.
//!
//! Every call goes over the wire the real SDK uses — the data-plane calls as `POST /`
//! with `X-Amz-Target: ToggleCustomerAPI.<Op>` and an awsJson1_0 body, the control-plane
//! ones as the REST routes that create the resources — through the same
//! `MultiServiceRouter` the emulator boots. The safety rules are created through the
//! CONTROL plane, which is the only way one exists at all, and are waited to `DEPLOYED`
//! before they are expected to guard anything.
//!
//! The rule under test is asserted over a TABLE of inputs: both kinds of rule, every
//! value of the modelled `RuleType` enum, `Inverted` both ways, thresholds at and past
//! the number of controls a rule evaluates, a control no rule guards, and the single and
//! the batch update operation.
//!
//! The last two suites cover the configuration a client cannot talk its way out of: an
//! `ATLEAST` rule whose `Threshold` is past the number of controls it asserts, which no
//! state vector satisfies, and the fact that enforcement tracks the control plane's
//! current rule set — a deleted rule guards nothing, a re-created one guards again.

use serde_json::{json, Value};

use super::common::{
    call, create_assertion_rule, create_control_panel, create_cluster, create_gating_rule,
    create_routing_control, emulator, error_code, error_message, get_state, Control, Panel,
    RuleConfigSpec,
};
use smithy_server_runtime::MultiServiceRouter;

/// A fresh emulator with one control panel holding `count` routing controls, all `Off`:
/// the fixture every row below deploys its own rule into.
async fn panel_with_controls(count: usize) -> (MultiServiceRouter, Panel, Vec<Control>) {
    let router = emulator();
    let cluster = create_cluster(&router, "safety-rules").await;
    let mut panel = create_control_panel(&router, &cluster, "safety-rules-panel").await;
    let mut controls = Vec::new();
    for index in 0..count {
        let control = create_routing_control(&router, &panel, &format!("control-{index}")).await;
        controls.push(control.clone());
        panel.controls.push(control);
    }
    (router, panel, controls)
}

/// Drive one routing control's state, expecting the update to be ACCEPTED.
async fn set_state(router: &MultiServiceRouter, case: &str, arn: &str, state: &str) {
    let (status, body) = call(
        router,
        "UpdateRoutingControlState",
        &json!({ "RoutingControlArn": arn, "RoutingControlState": state }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: setting {arn} to {state} must be accepted, got {status} with {body}"
    );
}

/// The state `GetRoutingControlState` reports for `arn`.
async fn reported_state(router: &MultiServiceRouter, arn: &str) -> String {
    get_state(router, arn)
        .await
        .get("RoutingControlState")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `UpdateRoutingControlState`, optionally overriding safety rules, as `(status, body)`.
async fn update(
    router: &MultiServiceRouter,
    arn: &str,
    state: &str,
    overrides: &[String],
) -> (u16, Value) {
    let mut request = serde_json::Map::new();
    request.insert("RoutingControlArn".to_string(), json!(arn));
    request.insert("RoutingControlState".to_string(), json!(state));
    if !overrides.is_empty() {
        request.insert("SafetyRulesToOverride".to_string(), json!(overrides));
    }
    call(router, "UpdateRoutingControlState", &Value::Object(request)).await
}

/// `UpdateRoutingControlStates`, optionally overriding safety rules, as `(status, body)`.
async fn update_batch(
    router: &MultiServiceRouter,
    entries: &[(String, &str)],
    overrides: &[String],
) -> (u16, Value) {
    let mut request = serde_json::Map::new();
    request.insert(
        "UpdateRoutingControlStateEntries".to_string(),
        json!(entries
            .iter()
            .map(|(arn, state)| json!({
                "RoutingControlArn": arn,
                "RoutingControlState": state,
            }))
            .collect::<Vec<Value>>()),
    );
    if !overrides.is_empty() {
        request.insert("SafetyRulesToOverride".to_string(), json!(overrides));
    }
    call(router, "UpdateRoutingControlStates", &Value::Object(request)).await
}

/// Which kind of rule a row deploys, over which of the panel's controls (by index).
enum RuleSpec {
    /// An assertion rule over `asserted`.
    Assertion {
        asserted: &'static [usize],
        config: RuleConfigSpec,
    },
    /// A gating rule whose `gating` controls gate `targets`.
    Gating {
        gating: &'static [usize],
        targets: &'static [usize],
        config: RuleConfigSpec,
    },
}

/// One row of the enforcement table.
struct Row {
    label: &'static str,
    rule: RuleSpec,
    /// The states the panel's controls are driven to BEFORE the rule is deployed.
    initial: &'static [(usize, &'static str)],
    /// The update attempted once the rule is deployed.
    update: (usize, &'static str),
    /// Whether the deployed rule must refuse that update.
    refused: bool,
}

/// RULE: a deployed safety rule of a routing control's control panel is evaluated before
/// the control's state is written — an assertion rule over the state its
/// `AssertedControls` would be left in, a gating rule over the state its `GatingControls`
/// would be left in whenever the update touches one of its `TargetControls` — and an
/// update that would leave that rule's `RuleConfig` unsatisfied is refused with HTTP 400
/// `ValidationException` and writes nothing.
///
/// The rows span both kinds of rule, every value of the modelled `RuleType` enum
/// (`ATLEAST`, `AND`, `OR` — see `smithy_query(query="doc",shape="RuleType")` on the
/// control plane's model, which defines `AND` as "at least N, where N is the total number
/// of controls in the rule" and `OR` as "at least N, where N is 1"), `Inverted` both
/// ways, thresholds of 0, 1, the number of evaluated controls and one past it, an update
/// that SATISFIES each rule, an update to a gating control (which its own rule does not
/// gate) and an update to a control no rule names.
#[tokio::test]
async fn a_deployed_safety_rule_gates_the_updates_it_guards() {
    const ATLEAST_ONE: RuleConfigSpec = RuleConfigSpec {
        rule_type: "ATLEAST",
        threshold: 1,
        inverted: false,
    };
    const AND_ALL: RuleConfigSpec = RuleConfigSpec {
        rule_type: "AND",
        threshold: 2,
        inverted: false,
    };
    const ANY_ON: RuleConfigSpec = RuleConfigSpec {
        rule_type: "OR",
        threshold: 0,
        inverted: false,
    };
    const NONE_ON: RuleConfigSpec = RuleConfigSpec {
        rule_type: "OR",
        threshold: 0,
        inverted: true,
    };

    let rows: Vec<Row> = vec![
        Row {
            label: "ATLEAST 1 of 2 asserted: switching the last On control off",
            rule: RuleSpec::Assertion {
                asserted: &[0, 1],
                config: ATLEAST_ONE,
            },
            initial: &[(0, "On")],
            update: (0, "Off"),
            refused: true,
        },
        Row {
            label: "ATLEAST 1 of 2 asserted: switching the other control on",
            rule: RuleSpec::Assertion {
                asserted: &[0, 1],
                config: ATLEAST_ONE,
            },
            initial: &[(0, "On")],
            update: (1, "On"),
            refused: false,
        },
        Row {
            label: "ATLEAST 1 of 2 asserted: switching one of two On controls off",
            rule: RuleSpec::Assertion {
                asserted: &[0, 1],
                config: ATLEAST_ONE,
            },
            initial: &[(0, "On"), (1, "On")],
            update: (0, "Off"),
            refused: false,
        },
        Row {
            label: "ATLEAST 0 of 2 asserted: a threshold nothing can fall below",
            rule: RuleSpec::Assertion {
                asserted: &[0, 1],
                config: RuleConfigSpec {
                    rule_type: "ATLEAST",
                    threshold: 0,
                    inverted: false,
                },
            },
            initial: &[(0, "On")],
            update: (0, "Off"),
            refused: false,
        },
        Row {
            label: "ATLEAST 3 of 2 asserted: a threshold past every control it evaluates",
            rule: RuleSpec::Assertion {
                asserted: &[0, 1],
                config: RuleConfigSpec {
                    rule_type: "ATLEAST",
                    threshold: 3,
                    inverted: false,
                },
            },
            initial: &[(0, "On"), (1, "On")],
            update: (0, "On"),
            refused: true,
        },
        Row {
            label: "AND over 2 asserted: switching one of them off",
            rule: RuleSpec::Assertion {
                asserted: &[0, 1],
                config: AND_ALL,
            },
            initial: &[(0, "On"), (1, "On")],
            update: (0, "Off"),
            refused: true,
        },
        Row {
            label: "AND over 2 asserted: switching the missing one on",
            rule: RuleSpec::Assertion {
                asserted: &[0, 1],
                config: AND_ALL,
            },
            initial: &[(1, "On")],
            update: (0, "On"),
            refused: false,
        },
        Row {
            label: "OR over 2 asserted: switching the last On control off",
            rule: RuleSpec::Assertion {
                asserted: &[0, 1],
                config: ANY_ON,
            },
            initial: &[(0, "On")],
            update: (0, "Off"),
            refused: true,
        },
        Row {
            label: "inverted OR over 2 asserted: switching a control on",
            rule: RuleSpec::Assertion {
                asserted: &[0, 1],
                config: NONE_ON,
            },
            initial: &[],
            update: (0, "On"),
            refused: true,
        },
        Row {
            label: "inverted OR over 2 asserted: switching the On control off",
            rule: RuleSpec::Assertion {
                asserted: &[0, 1],
                config: NONE_ON,
            },
            initial: &[(0, "On")],
            update: (0, "Off"),
            refused: false,
        },
        Row {
            label: "ATLEAST 1 of 2 asserted: a control the rule does not name",
            rule: RuleSpec::Assertion {
                asserted: &[0, 1],
                config: ATLEAST_ONE,
            },
            initial: &[],
            update: (2, "On"),
            refused: false,
        },
        Row {
            label: "OR gate shut: switching the target on",
            rule: RuleSpec::Gating {
                gating: &[0],
                targets: &[1],
                config: ANY_ON,
            },
            initial: &[],
            update: (1, "On"),
            refused: true,
        },
        Row {
            label: "OR gate shut: switching the target off",
            rule: RuleSpec::Gating {
                gating: &[0],
                targets: &[1],
                config: ANY_ON,
            },
            initial: &[(1, "On")],
            update: (1, "Off"),
            refused: true,
        },
        Row {
            label: "OR gate shut: switching the GATE itself on",
            rule: RuleSpec::Gating {
                gating: &[0],
                targets: &[1],
                config: ANY_ON,
            },
            initial: &[],
            update: (0, "On"),
            refused: false,
        },
        Row {
            label: "OR gate open: switching the target on",
            rule: RuleSpec::Gating {
                gating: &[0],
                targets: &[1],
                config: ANY_ON,
            },
            initial: &[(0, "On")],
            update: (1, "On"),
            refused: false,
        },
        Row {
            label: "AND over 2 gates, one shut: switching the target on",
            rule: RuleSpec::Gating {
                gating: &[0, 1],
                targets: &[2],
                config: AND_ALL,
            },
            initial: &[(0, "On")],
            update: (2, "On"),
            refused: true,
        },
        Row {
            label: "AND over 2 gates, both open: switching the target on",
            rule: RuleSpec::Gating {
                gating: &[0, 1],
                targets: &[2],
                config: AND_ALL,
            },
            initial: &[(0, "On"), (1, "On")],
            update: (2, "On"),
            refused: false,
        },
        Row {
            label: "inverted OR gate: switching the target on while a gate is On",
            rule: RuleSpec::Gating {
                gating: &[0],
                targets: &[1],
                config: NONE_ON,
            },
            initial: &[(0, "On")],
            update: (1, "On"),
            refused: true,
        },
        Row {
            label: "inverted OR gate: switching the target on while every gate is Off",
            rule: RuleSpec::Gating {
                gating: &[0],
                targets: &[1],
                config: NONE_ON,
            },
            initial: &[],
            update: (1, "On"),
            refused: false,
        },
        Row {
            label: "OR gate shut: a control that is neither gate nor target",
            rule: RuleSpec::Gating {
                gating: &[0],
                targets: &[1],
                config: ANY_ON,
            },
            initial: &[],
            update: (2, "On"),
            refused: false,
        },
    ];

    for row in &rows {
        let case = row.label;
        let (router, panel, controls) = panel_with_controls(3).await;

        // The fixture is driven into place BEFORE the rule is deployed, so the setup is
        // never the thing the rule refuses.
        for (index, state) in row.initial {
            set_state(&router, case, &controls[*index].arn, state).await;
        }

        let rule_arn = match &row.rule {
            RuleSpec::Assertion { asserted, config } => {
                let asserted: Vec<String> =
                    asserted.iter().map(|i| controls[*i].arn.clone()).collect();
                create_assertion_rule(&router, &panel.arn, "rule", &asserted, *config).await
            }
            RuleSpec::Gating {
                gating,
                targets,
                config,
            } => {
                let gating: Vec<String> =
                    gating.iter().map(|i| controls[*i].arn.clone()).collect();
                let targets: Vec<String> =
                    targets.iter().map(|i| controls[*i].arn.clone()).collect();
                create_gating_rule(&router, &panel.arn, "rule", &gating, &targets, *config).await
            }
        };

        let (index, requested) = row.update;
        let arn = &controls[index].arn;
        let before = reported_state(&router, arn).await;
        let (status, body) = update(&router, arn, requested, &[]).await;

        if row.refused {
            assert_eq!(
                status, 400,
                "case {case}: expected 400, got {status} with {body}"
            );
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case}: expected ValidationException, got {body}"
            );
            // The refusal names the rule that blocked it: that ARN is what a caller puts
            // in `SafetyRulesToOverride`.
            assert!(
                error_message(&body).contains(&rule_arn),
                "case {case}: the refusal must name the safety rule {rule_arn}, got {body}"
            );
            let after = reported_state(&router, arn).await;
            assert_eq!(
                after, before,
                "case {case}: a refused update must not move {arn}, got {after}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case}: expected 200, got {status} with {body}"
            );
            let after = reported_state(&router, arn).await;
            assert_eq!(
                after, requested,
                "case {case}: an accepted update must move {arn} to {requested}, got {after}"
            );
        }
    }
}

/// RULE: a safety rule whose ARN the request lists in `SafetyRulesToOverride` is left out
/// of the evaluation for that one call, so the update it would otherwise refuse is
/// accepted and the state really moves; every rule the request does NOT name is still
/// evaluated and still refuses.
///
/// The rows are every subset of two deployed rules that the same update violates:
/// neither overridden, each one overridden alone, both overridden — plus an ARN that
/// names no rule at all.
#[tokio::test]
async fn an_overridden_safety_rule_is_not_evaluated_for_that_call() {
    let (router, panel, controls) = panel_with_controls(2).await;
    set_state(&router, "setup", &controls[0].arn, "On").await;

    // Two rules the same update (control 0 -> Off) violates: "at least one of the two
    // must stay On" and "control 0 itself must be On".
    let both = create_assertion_rule(
        &router,
        &panel.arn,
        "at-least-one-of-both",
        &[controls[0].arn.clone(), controls[1].arn.clone()],
        RuleConfigSpec {
            rule_type: "ATLEAST",
            threshold: 1,
            inverted: false,
        },
    )
    .await;
    let first_only = create_assertion_rule(
        &router,
        &panel.arn,
        "first-must-be-on",
        &[controls[0].arn.clone()],
        RuleConfigSpec {
            rule_type: "AND",
            threshold: 1,
            inverted: false,
        },
    )
    .await;
    let unknown_rule = format!("{}/safetyrule/00000000-0000-0000-0000-000000000000", panel.arn);

    let refused: Vec<(&str, Vec<String>)> = vec![
        ("no override at all", vec![]),
        ("only the two-control rule overridden", vec![both.clone()]),
        ("only the one-control rule overridden", vec![first_only.clone()]),
        ("a rule ARN that names no rule", vec![unknown_rule]),
    ];

    for (case, overrides) in &refused {
        let (status, body) = update(&router, &controls[0].arn, "Off", overrides).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case}: expected ValidationException, got {body}"
        );
        let state = reported_state(&router, &controls[0].arn).await;
        assert_eq!(
            state, "On",
            "case {case}: a refused update must leave the control On, got {state}"
        );
    }

    // Both rules overridden: the break-glass change goes through and the control really
    // moves.
    let overrides = vec![both.clone(), first_only.clone()];
    let (status, body) = update(&router, &controls[0].arn, "Off", &overrides).await;
    assert_eq!(
        status, 200,
        "case both rules overridden: expected 200, got {status} with {body}"
    );
    let state = reported_state(&router, &controls[0].arn).await;
    assert_eq!(
        state, "Off",
        "case both rules overridden: the control must have moved to Off, got {state}"
    );

    // An override is per-call, not a change to the rules: once the control is restored,
    // the same violating update is refused again when nothing is overridden.
    let (status, body) = update(&router, &controls[0].arn, "On", &[]).await;
    assert_eq!(
        status, 200,
        "case restoring the first control: expected 200, got {status} with {body}"
    );
    let state = reported_state(&router, &controls[0].arn).await;
    assert_eq!(
        state, "On",
        "case restoring the first control: expected On, got {state}"
    );
    let (status, body) = update(&router, &controls[0].arn, "Off", &[]).await;
    assert_eq!(
        status, 400,
        "case the same update without the override: expected 400, got {status} with {body}"
    );
    let state = reported_state(&router, &controls[0].arn).await;
    assert_eq!(
        state, "On",
        "case the same update without the override: expected On, got {state}"
    );
}

/// RULE: `UpdateRoutingControlStates` is one transaction, so a deployed rule is evaluated
/// over the state the WHOLE entry set would leave its controls in — a batch can satisfy a
/// rule no single entry of it could — and a batch a rule refuses commits no entry at all.
#[tokio::test]
async fn a_batch_update_is_gated_as_one_transaction() {
    let (router, panel, controls) = panel_with_controls(3).await;
    let arns: Vec<String> = controls.iter().map(|c| c.arn.clone()).collect();

    // "Both of these two controls must be On after the transaction."
    let rule = create_assertion_rule(
        &router,
        &panel.arn,
        "both-on",
        &[arns[0].clone(), arns[1].clone()],
        RuleConfigSpec {
            rule_type: "AND",
            threshold: 2,
            inverted: false,
        },
    )
    .await;

    // One entry on its own cannot satisfy the rule, and commits nothing.
    let (status, body) = update_batch(&router, &[(arns[0].clone(), "On")], &[]).await;
    assert_eq!(
        status, 400,
        "case one entry of two: expected 400, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "case one entry of two: expected ValidationException, got {body}"
    );
    for arn in &arns {
        let state = reported_state(&router, arn).await;
        assert_eq!(
            state, "Off",
            "case one entry of two: a refused batch must commit nothing, got {state} for {arn}"
        );
    }

    // The same two writes in ONE batch satisfy the rule, and both land.
    let (status, body) = update_batch(
        &router,
        &[(arns[0].clone(), "On"), (arns[1].clone(), "On")],
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case both entries together: expected 200, got {status} with {body}"
    );
    for arn in &arns[..2] {
        let state = reported_state(&router, arn).await;
        assert_eq!(
            state, "On",
            "case both entries together: every entry must be applied, got {state} for {arn}"
        );
    }

    // A control the rule does not name is not gated by it.
    let (status, body) = update_batch(&router, &[(arns[2].clone(), "On")], &[]).await;
    assert_eq!(
        status, 200,
        "case a control no rule names: expected 200, got {status} with {body}"
    );

    // Taking both controls back down violates the rule, and neither entry is committed.
    let down = [(arns[0].clone(), "Off"), (arns[1].clone(), "Off")];
    let (status, body) = update_batch(&router, &down, &[]).await;
    assert_eq!(
        status, 400,
        "case both entries down: expected 400, got {status} with {body}"
    );
    for arn in &arns[..2] {
        let state = reported_state(&router, arn).await;
        assert_eq!(
            state, "On",
            "case both entries down: a refused batch must commit nothing, got {state} for {arn}"
        );
    }

    // Overriding the rule lets the same batch through, and both entries land.
    let (status, body) = update_batch(&router, &down, std::slice::from_ref(&rule)).await;
    assert_eq!(
        status, 200,
        "case both entries down with the rule overridden: expected 200, got {status} with {body}"
    );
    for arn in &arns[..2] {
        let state = reported_state(&router, arn).await;
        assert_eq!(
            state, "Off",
            "case both entries down with the rule overridden: expected Off, got {state} for {arn}"
        );
    }
}

/// RULE: a gating rule orders the two updates — while the gate does not satisfy the rule
/// the target cannot be moved, the gate itself is not gated by its own rule, and once the
/// gate satisfies it the identical target update is accepted.
#[tokio::test]
async fn a_gating_rule_orders_the_gate_before_its_target() {
    let (router, panel, controls) = panel_with_controls(2).await;
    let gate = controls[0].arn.clone();
    let target = controls[1].arn.clone();

    let rule = create_gating_rule(
        &router,
        &panel.arn,
        "gate-before-target",
        std::slice::from_ref(&gate),
        std::slice::from_ref(&target),
        RuleConfigSpec {
            rule_type: "OR",
            threshold: 0,
            inverted: false,
        },
    )
    .await;

    // Beat 1: the gate is Off, so the target cannot be moved.
    let (status, body) = update(&router, &target, "On", &[]).await;
    assert_eq!(
        status, 400,
        "case the gate is shut: expected 400, got {status} with {body}"
    );
    assert!(
        error_message(&body).contains(&rule),
        "case the gate is shut: the refusal must name the gating rule {rule}, got {body}"
    );
    let state = reported_state(&router, &target).await;
    assert_eq!(
        state, "Off",
        "case the gate is shut: the target must not have moved, got {state}"
    );

    // Beat 2: the gate is not a target of its own rule, so opening it is accepted.
    set_state(&router, "opening the gate", &gate, "On").await;
    let state = reported_state(&router, &gate).await;
    assert_eq!(
        state, "On",
        "case opening the gate: the gate must report On, got {state}"
    );

    // Beat 3: the identical target update is now accepted.
    let (status, body) = update(&router, &target, "On", &[]).await;
    assert_eq!(
        status, 200,
        "case the gate is open: expected 200, got {status} with {body}"
    );
    let state = reported_state(&router, &target).await;
    assert_eq!(
        state, "On",
        "case the gate is open: the target must report On, got {state}"
    );

    // Beat 4: shutting the gate again is accepted, and gates the target once more.
    set_state(&router, "shutting the gate", &gate, "Off").await;
    let (status, body) = update(&router, &target, "Off", &[]).await;
    assert_eq!(
        status, 400,
        "case the gate is shut again: expected 400, got {status} with {body}"
    );

    // Beat 5: one batch that opens the gate and moves the target is one transaction, so
    // the rule reads the state the gate would be left in.
    let (status, body) = update_batch(
        &router,
        &[(gate.clone(), "On"), (target.clone(), "Off")],
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case gate and target in one batch: expected 200, got {status} with {body}"
    );
    for (arn, expected) in [(&gate, "On"), (&target, "Off")] {
        let state = reported_state(&router, arn).await;
        assert_eq!(
            state, expected,
            "case gate and target in one batch: expected {expected} for {arn}, got {state}"
        );
    }
}

/// The three parts the refusal of an update a safety rule blocks must carry, for a rule
/// whose condition is "at least `required_on` of the controls it evaluates must be On":
/// the quoted condition, the routing control the request named and the rule that blocked
/// it. The whole sentence is pinned, character for character, by
/// `crate::safety_rule_refusal_message`; a row here states the three VARIABLE parts it
/// renders from the stored `RuleConfig`, the request and the rule.
fn refusal_parts(required_on: i32, routing_control_arn: &str, safety_rule_arn: &str) -> [String; 3] {
    [
        format!("\"At least {required_on} routing controls must be On.\""),
        format!("Routing control: [{routing_control_arn}]"),
        format!("Safety rule: {safety_rule_arn}"),
    ]
}

/// Assert that `body` is the refusal `safety_rule_arn` answers an update of
/// `routing_control_arn` with, for a rule that needs `required_on` controls On.
fn assert_refused(
    case: &str,
    status: u16,
    body: &Value,
    required_on: i32,
    routing_control_arn: &str,
    safety_rule_arn: &str,
) {
    assert_eq!(
        status, 400,
        "case {case}: expected 400, got {status} with {body}"
    );
    assert_eq!(
        error_code(body),
        "ValidationException",
        "case {case}: expected ValidationException, got {body}"
    );
    let actual = error_message(body);
    for part in refusal_parts(required_on, routing_control_arn, safety_rule_arn) {
        assert!(
            actual.contains(&part),
            "case {case}: the refusal must state {part:?}, got {actual:?}"
        );
    }
}

/// RULE: a safety rule's `RuleConfig` is evaluated exactly as stored, so an `ATLEAST`
/// assertion rule whose `Threshold` is greater than the number of controls it asserts is
/// satisfied by no state vector at all: every update of every control it asserts is
/// refused with HTTP 400 `ValidationException` naming that rule, in either direction and
/// through either update operation, and nothing is written. A control the rule does not
/// assert is not affected by it, and listing the rule's ARN in `SafetyRulesToOverride` is
/// the one way through.
///
/// The control plane accepts such a rule — `CreateSafetyRule` validates `Threshold`
/// against nothing (`smithy_query(query="shape",shape="RuleConfig")` on the control
/// plane's model constrains neither member) — so the wedge is a configuration a client
/// can really get into, and the rows span it: 1, 2 and 3 asserted controls, each with a
/// `Threshold` one and two past that count, every asserted control as the one updated,
/// and both values of the modelled `RoutingControlState` enum as the state requested.
#[tokio::test]
async fn an_unsatisfiable_threshold_refuses_every_update_of_the_controls_it_asserts() {
    for asserted_count in 1..=3usize {
        for past_the_count in 1..=2i32 {
            let threshold = asserted_count as i32 + past_the_count;
            let row = format!("ATLEAST {threshold} over {asserted_count} asserted controls");

            // One control more than the rule asserts, so every row also carries a control
            // no rule names.
            let (router, panel, controls) = panel_with_controls(asserted_count + 1).await;
            let asserted: Vec<String> = controls[..asserted_count]
                .iter()
                .map(|control| control.arn.clone())
                .collect();
            let unguarded = controls[asserted_count].arn.clone();
            let rule = create_assertion_rule(
                &router,
                &panel.arn,
                "wedged",
                &asserted,
                RuleConfigSpec {
                    rule_type: "ATLEAST",
                    threshold,
                    inverted: false,
                },
            )
            .await;

            for arn in &asserted {
                for requested in ["On", "Off"] {
                    let before = reported_state(&router, arn).await;

                    let case = format!("{row}: {arn} to {requested}");
                    let (status, body) = update(&router, arn, requested, &[]).await;
                    assert_refused(&case, status, &body, threshold, arn, &rule);

                    // The same update as a one-entry batch: the stage that refuses it is
                    // the same one, so the answer is the same.
                    let batch_case = format!("{row}: {arn} to {requested} as a batch");
                    let (status, body) =
                        update_batch(&router, &[(arn.clone(), requested)], &[]).await;
                    assert_refused(&batch_case, status, &body, threshold, arn, &rule);

                    let after = reported_state(&router, arn).await;
                    assert_eq!(
                        after, before,
                        "case {case}: a refused update must not move {arn}, got {after}"
                    );
                }
            }

            // A control the wedged rule does not assert is not gated by it.
            for requested in ["On", "Off"] {
                let case = format!("{row}: the unasserted control to {requested}");
                let (status, body) = update(&router, &unguarded, requested, &[]).await;
                assert_eq!(
                    status, 200,
                    "case {case}: expected 200, got {status} with {body}"
                );
                let after = reported_state(&router, &unguarded).await;
                assert_eq!(
                    after, requested,
                    "case {case}: the update must move {unguarded} to {requested}, got {after}"
                );
            }

            // Overriding the wedged rule is the break-glass way through it, and the state
            // really moves.
            let arn = &asserted[0];
            let case = format!("{row}: {arn} to On with the rule overridden");
            let (status, body) = update(&router, arn, "On", std::slice::from_ref(&rule)).await;
            assert_eq!(
                status, 200,
                "case {case}: expected 200, got {status} with {body}"
            );
            let after = reported_state(&router, arn).await;
            assert_eq!(
                after, "On",
                "case {case}: the overridden update must move {arn} to On, got {after}"
            );
        }
    }
}

/// RULE: enforcement reads the control plane's CURRENT safety rules rather than a copy
/// taken when a rule was created, so an update is refused exactly while a rule that
/// refuses it exists: once `DeleteSafetyRule` has removed it — the point from which
/// `ListSafetyRules` no longer reports it — the identical update is accepted, and a rule
/// created again refuses that identical update again.
///
/// The ARN of a rule that no longer exists in `SafetyRulesToOverride` overrides nothing,
/// which is the same answer an ARN that never named a rule gets.
#[tokio::test]
async fn enforcement_follows_the_control_plane_rule_set() {
    use super::common::{control_plane, encode_arn};

    /// Whether `ListSafetyRules` still reports `safety_rule_arn` for `control_panel_arn`.
    async fn listed(
        router: &MultiServiceRouter,
        control_panel_arn: &str,
        safety_rule_arn: &str,
    ) -> bool {
        let (status, body) = control_plane(
            router,
            "GET",
            &format!("/controlpanel/{}/safetyrules", encode_arn(control_panel_arn)),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case ListSafetyRules({control_panel_arn}): expected 200, got {status} with {body}"
        );
        body.get("SafetyRules")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case ListSafetyRules: a page carries SafetyRules, got {body}"))
            .iter()
            // The modelled `Rule` shape carries each rule under the member its own kind
            // fills: `ASSERTION` or `GATING`.
            .filter_map(|rule| rule.get("ASSERTION").or_else(|| rule.get("GATING")))
            .any(|rule| rule.get("SafetyRuleArn").and_then(Value::as_str) == Some(safety_rule_arn))
    }

    let (router, panel, controls) = panel_with_controls(2).await;
    let arn = controls[0].arn.clone();
    let wedged = RuleConfigSpec {
        rule_type: "ATLEAST",
        threshold: 3,
        inverted: false,
    };
    let asserted = vec![arn.clone(), controls[1].arn.clone()];

    let first = create_assertion_rule(&router, &panel.arn, "wedged", &asserted, wedged).await;
    assert!(
        listed(&router, &panel.arn, &first).await,
        "case the rule is created: ListSafetyRules must report {first}"
    );
    let (status, body) = update(&router, &arn, "On", &[]).await;
    assert_refused("the rule exists", status, &body, 3, &arn, &first);

    let (status, body) = control_plane(
        &router,
        "DELETE",
        &format!("/safetyrule/{}", encode_arn(&first)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case DeleteSafetyRule({first}): expected 200, got {status} with {body}"
    );
    // The deletion is waited for by the CONDITION it establishes, not by a clock: the rule
    // stops guarding from the point the control plane stops reporting it.
    let mut gone = false;
    for _ in 0..600 {
        if !listed(&router, &panel.arn, &first).await {
            gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        gone,
        "case the rule is deleted: ListSafetyRules kept reporting {first}"
    );

    let (status, body) = update(&router, &arn, "On", &[]).await;
    assert_eq!(
        status, 200,
        "case the rule is deleted: expected 200, got {status} with {body}"
    );
    let after = reported_state(&router, &arn).await;
    assert_eq!(
        after, "On",
        "case the rule is deleted: the update must move {arn} to On, got {after}"
    );

    // A rule created again refuses the identical update again, and the deleted rule's ARN
    // overrides nothing.
    let second = create_assertion_rule(&router, &panel.arn, "wedged-again", &asserted, wedged).await;
    assert_ne!(
        second, first,
        "case the rule is created again: a second create mints its own ARN, got {second}"
    );
    let (status, body) = update(&router, &arn, "Off", std::slice::from_ref(&first)).await;
    assert_refused(
        "the deleted rule's ARN as the override",
        status,
        &body,
        3,
        &arn,
        &second,
    );
    let after = reported_state(&router, &arn).await;
    assert_eq!(
        after, "On",
        "case the deleted rule's ARN as the override: {arn} must not have moved, got {after}"
    );

    let (status, body) = update(&router, &arn, "Off", std::slice::from_ref(&second)).await;
    assert_eq!(
        status, 200,
        "case the new rule overridden: expected 200, got {status} with {body}"
    );
    let after = reported_state(&router, &arn).await;
    assert_eq!(
        after, "Off",
        "case the new rule overridden: the update must move {arn} to Off, got {after}"
    );
}
