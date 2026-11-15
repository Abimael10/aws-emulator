// SPDX-License-Identifier: CC0-1.0

//! The sentence this service refuses a name already taken inside the parent that scopes it
//! with.
//!
//! The rule under test: a create or a rename that asks for a name another resource of the
//! same kind already holds inside the parent that scopes it is refused with
//! `ValidationException` (400) whose `message` names the KIND of resource, the
//! server-chosen id of the resource that already holds the name, and the name the request
//! asked for — `A <kind> with the specified name already exists: <existing id>, <name>` —
//! so a caller can tell a name collision from the other causes this error class
//! multiplexes, and can read the colliding resource and the offending name out of it.
//! The parent the name is scoped to is NOT part of the sentence, and the colliding
//! resource is named by its id, never by an ARN.
//!
//! The tables span what that rule generalises over: every resource kind whose name is
//! scoped to a parent (control panel in its cluster, routing control in its control panel,
//! safety rule in its control panel), both request paths that take a name (the create and
//! the rename), both control panels a routing control can land in (a named one and the
//! default panel reached by omitting `ControlPanelArn`), and several fillings of the name
//! itself, including both published `@length` bounds.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::common::{
    call, create_cluster, create_control_panel, create_routing_control, error_code, router,
};

/// The published `@length` maximum of every name member of this service
/// (`smithy_query(query="operation",operation="CreateRoutingControl")` ->
/// `__stringMin1Max64PatternS`, len 1..64, pattern `^\S+$`).
const NAME_MAX_LENGTH: usize = 64;

/// The `message` of an error body, under the `message` jsonName the model gives this
/// service's error shapes (`smithy_query(query="shape",shape="ValidationException")`).
fn message(body: &Value) -> String {
    body["message"].as_str().unwrap_or_default().to_string()
}

/// The sentence the rule requires, written from the rule rather than from the service's
/// own helper: the kind of resource, the id of the resource that already holds the name,
/// then the name the request asked for.
fn already_exists(kind: &str, existing_arn: &str, requested: &str) -> String {
    let existing_id = existing_arn
        .rsplit_once('/')
        .map_or(existing_arn, |(_, id)| id);
    format!("A {kind} with the specified name already exists: {existing_id}, {requested}")
}

/// The names to drive the rule with: the published minimum length, a plain name, a name
/// holding every other character class the `^\S+$` pattern allows, and the published
/// maximum length.
fn names() -> Vec<(&'static str, String)> {
    vec![
        ("the published minimum length", "x".to_string()),
        ("a plain name", "dup-name".to_string()),
        (
            "every other character class the pattern allows",
            "Dup.Name_1:2/3+%@~".to_string(),
        ),
        (
            "the published maximum length",
            "d".repeat(NAME_MAX_LENGTH),
        ),
    ]
}

/// Assert one refused request: 400, `ValidationException`, and the sentence the rule
/// requires for `kind`, `existing_arn` and `requested`.
fn assert_refused(
    case: &str,
    status: u16,
    answer: &Value,
    kind: &str,
    existing_arn: &str,
    requested: &str,
) {
    assert_eq!(
        status, 400,
        "case {case}: expected 400, got {status} {answer:?}"
    );
    assert_eq!(
        error_code(answer),
        "ValidationException",
        "case {case}: expected ValidationException, got {answer:?}"
    );
    let actual = message(answer);
    let expected = already_exists(kind, existing_arn, requested);
    assert_eq!(
        actual, expected,
        "case {case}: expected {expected:?}, got {actual:?}"
    );
    assert!(
        !actual.contains("arn:"),
        "case {case}: the refusal must name the colliding resource by its id, not by an \
         ARN, got {actual:?}"
    );
}

/// Call `UpdateRoutingControl` for `arn` and `name`.
async fn rename_routing_control(
    router: &MultiServiceRouter,
    arn: &str,
    name: &str,
) -> (u16, Value) {
    call(
        router,
        "PUT",
        "/routingcontrol",
        Some(json!({ "RoutingControlArn": arn, "RoutingControlName": name })),
    )
    .await
}

/// Call `UpdateControlPanel` for `arn` and `name`.
async fn rename_control_panel(router: &MultiServiceRouter, arn: &str, name: &str) -> (u16, Value) {
    call(
        router,
        "PUT",
        "/controlpanel",
        Some(json!({ "ControlPanelArn": arn, "ControlPanelName": name })),
    )
    .await
}

/// A `NewAssertionRule` body for one control panel, control and name.
fn assertion_rule(panel_arn: &str, control_arn: &str, name: &str) -> Value {
    json!({
        "AssertionRule": {
            "AssertedControls": [control_arn],
            "ControlPanelArn": panel_arn,
            "Name": name,
            "RuleConfig": { "Inverted": false, "Threshold": 1, "Type": "ATLEAST" },
            "WaitPeriodMs": 5000
        }
    })
}

/// RULE: a `CreateRoutingControl` for a name the target control panel already holds is
/// refused with the sentence naming the COLLIDING control's id and the requested name —
/// in a named panel and in the cluster's default panel alike, with the same name free in
/// each of them because it is scoped to one panel.
#[tokio::test]
async fn a_duplicate_routing_control_name_is_refused_by_the_colliding_controls_id() {
    let router = router();
    let cluster = create_cluster(&router, json!({ "ClusterName": "duplicate-names" })).await;
    let cluster_arn = cluster["ClusterArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let panel = create_control_panel(
        &router,
        json!({ "ClusterArn": cluster_arn, "ControlPanelName": "named-panel" }),
    )
    .await;
    let named_panel_arn = panel["ControlPanelArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    for (case, name) in names() {
        // Rows: the panel named by the request, and the default panel the request lands in
        // by omitting `ControlPanelArn`.
        for scope in ["a named control panel", "the default control panel"] {
            let mut body = json!({ "ClusterArn": cluster_arn, "RoutingControlName": name });
            if scope == "a named control panel" {
                body["ControlPanelArn"] = json!(named_panel_arn);
            }
            let winner = create_routing_control(&router, body.clone()).await;
            let winner_arn = winner["RoutingControlArn"]
                .as_str()
                .unwrap_or_default()
                .to_string();

            let (status, answer) = call(&router, "POST", "/routingcontrol", Some(body)).await;
            assert_refused(
                &format!("{case} in {scope}"),
                status,
                &answer,
                "routing control",
                &winner_arn,
                &name,
            );
        }
    }
}

/// RULE, on the rename path: `UpdateRoutingControl` onto a name another control of the
/// same control panel holds is refused with the same sentence, naming the control that
/// holds the name rather than the one being renamed.
#[tokio::test]
async fn a_rename_onto_a_taken_routing_control_name_names_the_holder() {
    let router = router();
    let cluster = create_cluster(&router, json!({ "ClusterName": "rename-collisions" })).await;
    let cluster_arn = cluster["ClusterArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let panel = create_control_panel(
        &router,
        json!({ "ClusterArn": cluster_arn, "ControlPanelName": "rename-panel" }),
    )
    .await;
    let panel_arn = panel["ControlPanelArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    for (index, (case, name)) in names().into_iter().enumerate() {
        let holder = create_routing_control(
            &router,
            json!({
                "ClusterArn": cluster_arn,
                "ControlPanelArn": panel_arn,
                "RoutingControlName": name,
            }),
        )
        .await;
        let holder_arn = holder["RoutingControlArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let mover = create_routing_control(
            &router,
            json!({
                "ClusterArn": cluster_arn,
                "ControlPanelArn": panel_arn,
                "RoutingControlName": format!("mover-{index}"),
            }),
        )
        .await;
        let mover_arn = mover["RoutingControlArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let (status, answer) = rename_routing_control(&router, &mover_arn, &name).await;
        assert_refused(
            &format!("rename onto {case}"),
            status,
            &answer,
            "routing control",
            &holder_arn,
            &name,
        );
    }
}

/// RULE, at the control-panel level: a create and a rename onto a name another panel of
/// the same cluster holds are both refused with the sentence naming the colliding PANEL's
/// id and the requested name.
#[tokio::test]
async fn a_duplicate_control_panel_name_is_refused_by_the_colliding_panels_id() {
    let router = router();
    let cluster = create_cluster(&router, json!({ "ClusterName": "panel-names" })).await;
    let cluster_arn = cluster["ClusterArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    for (index, (case, name)) in names().into_iter().enumerate() {
        let holder = create_control_panel(
            &router,
            json!({ "ClusterArn": cluster_arn, "ControlPanelName": name }),
        )
        .await;
        let holder_arn = holder["ControlPanelArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let (status, answer) = call(
            &router,
            "POST",
            "/controlpanel",
            Some(json!({ "ClusterArn": cluster_arn, "ControlPanelName": name })),
        )
        .await;
        assert_refused(
            &format!("create a second panel under {case}"),
            status,
            &answer,
            "control panel",
            &holder_arn,
            &name,
        );

        let mover = create_control_panel(
            &router,
            json!({
                "ClusterArn": cluster_arn,
                "ControlPanelName": format!("panel-mover-{index}")
            }),
        )
        .await;
        let mover_arn = mover["ControlPanelArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let (status, answer) = rename_control_panel(&router, &mover_arn, &name).await;
        assert_refused(
            &format!("rename a panel onto {case}"),
            status,
            &answer,
            "control panel",
            &holder_arn,
            &name,
        );
    }
}

/// RULE, at the safety-rule level: a create and a rename onto a name another rule of the
/// same control panel holds are both refused with the sentence naming the colliding
/// RULE's id and the requested name.
#[tokio::test]
async fn a_duplicate_safety_rule_name_is_refused_by_the_colliding_rules_id() {
    let router = router();
    let (panel_arn, controls) = super::common::panel_with_controls(&router, "rule-names", 1).await;

    for (index, (case, name)) in names().into_iter().enumerate() {
        let (status, created) = call(
            &router,
            "POST",
            "/safetyrule",
            Some(assertion_rule(&panel_arn, &controls[0], &name)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case the first rule under {case}: expected 200, got {status} {created:?}"
        );
        let holder_arn = created["AssertionRule"]["SafetyRuleArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let (status, answer) = call(
            &router,
            "POST",
            "/safetyrule",
            Some(assertion_rule(&panel_arn, &controls[0], &name)),
        )
        .await;
        assert_refused(
            &format!("create a second rule under {case}"),
            status,
            &answer,
            "safety rule",
            &holder_arn,
            &name,
        );

        let (status, mover) = call(
            &router,
            "POST",
            "/safetyrule",
            Some(assertion_rule(
                &panel_arn,
                &controls[0],
                &format!("rule-mover-{index}"),
            )),
        )
        .await;
        assert_eq!(
            status, 200,
            "case a rule to rename for {case}: expected 200, got {status} {mover:?}"
        );
        let mover_arn = mover["AssertionRule"]["SafetyRuleArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let (status, answer) = call(
            &router,
            "PUT",
            "/safetyrule",
            Some(json!({
                "AssertionRuleUpdate": {
                    "Name": name,
                    "SafetyRuleArn": mover_arn,
                    "WaitPeriodMs": 5000
                }
            })),
        )
        .await;
        assert_refused(
            &format!("rename a rule onto {case}"),
            status,
            &answer,
            "safety rule",
            &holder_arn,
            &name,
        );
    }
}
