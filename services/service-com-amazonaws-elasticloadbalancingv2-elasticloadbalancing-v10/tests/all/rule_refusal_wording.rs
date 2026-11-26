// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS three refusals a rule request earns, driven over the wire exactly
//! as a client reads them.
//!
//! An error code and an error message are both part of the contract: a client branches on the
//! code and an operator reads the message, so each of these says what Elastic Load Balancing
//! says rather than what the member names of the API are called.
//!
//! Three rules are asserted here, each over a SET of inputs:
//!
//! * an action list naming two actions that cannot be performed together is refused naming
//!   the TYPES that clash, comma-joined — the LAST routing action of the list, which is the one
//!   a rule performs, then the one before it, which is what the caller has to drop — and a
//!   list naming no routing action at all, which has no type of the request to name, states
//!   the types a rule may end with instead;
//! * a rule read naming neither selector is refused about the two SELECTORS it could have
//!   named;
//! * a re-prioritisation naming a rule the store does not hold is refused about the RULES it
//!   named, whatever else the batch asks for, and the rules it does hold keep their
//!   priorities.

use crate::listener_rules::{
    add_rule, create_listener, create_load_balancer, create_rule, fixed_response, rules_of, Form,
};
use crate::provisioning_loop::{action, call, create_http_target_group, emulator, p};
use crate::trust_store_lifecycle::message;
use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::state::{
    incompatible_action_types_message, rule_selection_not_found_message,
    rule_selector_required_message,
};
use smithy_server_runtime::MultiServiceRouter;

/// The error code this service answers an input its own documented rules refuse with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The awsQuery error code of `InvalidLoadBalancerActionException`, which every operation
/// that takes an action models
/// (`smithy_query(query="operation",operation="CreateRule")`).
const INVALID_ACTION: &str = "InvalidLoadBalancerAction";

/// The awsQuery error code of `RuleNotFoundException`, which `SetRulePriorities` models
/// (`smithy_query(query="operation",operation="SetRulePriorities")`: HTTP 400).
const RULE_NOT_FOUND: &str = "RuleNotFound";

/// A load balancer, a target group and one HTTP listener on `port`.
async fn listener_on(router: &MultiServiceRouter, name: &str, port: i32) -> String {
    let load_balancer = create_load_balancer(router, name).await;
    let group = create_http_target_group(router, &format!("{name}-tg")).await;
    create_listener(router, &load_balancer, &group, port).await
}

/// One path-pattern condition, so a rule row carries a condition at all.
fn path_pattern(path: &str) -> Form {
    vec![
        p("Conditions.member.1.Field", "path-pattern"),
        p("Conditions.member.1.PathPatternConfig.Values.member.1", path),
    ]
}

/// The `Actions.member.<position>` fields of one action of `kind`.
fn routing_action(position: usize, kind: &str) -> Form {
    let member = format!("Actions.member.{position}");
    match kind {
        "fixed-response" => vec![
            p(&format!("{member}.Type"), "fixed-response"),
            p(&format!("{member}.FixedResponseConfig.StatusCode"), "404"),
        ],
        "redirect" => vec![
            p(&format!("{member}.Type"), "redirect"),
            p(&format!("{member}.RedirectConfig.StatusCode"), "HTTP_301"),
            p(&format!("{member}.RedirectConfig.Host"), "moved.example.com"),
        ],
        "authenticate-cognito" => vec![
            p(&format!("{member}.Type"), "authenticate-cognito"),
            p(
                &format!("{member}.AuthenticateCognitoConfig.UserPoolArn"),
                "arn:aws:cognito-idp:us-west-2:000000000000:userpool/us-west-2_pool",
            ),
            p(
                &format!("{member}.AuthenticateCognitoConfig.UserPoolClientId"),
                "client-id",
            ),
            p(
                &format!("{member}.AuthenticateCognitoConfig.UserPoolDomain"),
                "auth.example.com",
            ),
        ],
        other => panic!("no action is spelled for the type {other:?}"),
    }
}

/// One row of the clashing-actions table: the case, the action TYPES the request lists in
/// order, and the types the refusal must name — `None` for a list that names no clash.
type ClashRow = (
    &'static str,
    &'static [&'static str],
    Option<&'static [&'static str]>,
);

/// RULE: an action list naming two actions that cannot be performed together is refused
/// naming those types: the last routing action of the list first, then the one before it.
#[tokio::test]
async fn two_routing_actions_are_refused_naming_the_types_that_clash() {
    let (router, _engine) = emulator();
    let listener = listener_on(&router, "clashing-actions", 80).await;

    // The rows span what the rule generalises over: WHICH two routing actions clash and in
    // which order the request listed them — the refusal names the LAST of them first, which
    // is the one a rule performs, and then the one the caller has to drop — plus the list that
    // names no clash at all, whose refusal is about something else.
    let rows: &[ClashRow] = &[
        (
            "a redirect and a fixed response",
            &["redirect", "fixed-response"],
            Some(&["fixed-response", "redirect"]),
        ),
        (
            "the mirror: the fixed response first",
            &["fixed-response", "redirect"],
            Some(&["redirect", "fixed-response"]),
        ),
        (
            "an authentication action in front of one routing action, which is no clash",
            &["authenticate-cognito", "fixed-response"],
            None,
        ),
    ];
    for (index, (case, kinds, clashing)) in rows.iter().enumerate() {
        let priority = i32::try_from(index + 10).expect("a rule priority");
        let mut fields = path_pattern(&format!("/clash-{index}"));
        for (position, kind) in kinds.iter().enumerate() {
            fields.extend(routing_action(position + 1, kind));
        }
        let before = rules_of(&router, case, &listener).await;
        let created = create_rule(&router, &listener, &priority.to_string(), &fields).await;
        match clashing {
            Some(expected) => {
                assert_eq!(
                    (created.status, created.error_code(), message(&created)),
                    (
                        400,
                        INVALID_ACTION.to_string(),
                        incompatible_action_types_message(expected)
                    ),
                    "case {case:?}: expected the refusal naming the clashing types, got {} {}",
                    created.status,
                    created.body
                );
                let after = rules_of(&router, case, &listener).await;
                assert_eq!(
                    after, before,
                    "case {case:?}: a refused create stores nothing, got {after:?}"
                );
            }
            None => assert_ne!(
                message(&created),
                incompatible_action_types_message(kinds),
                "case {case:?}: a list with one routing action names no clash, got {}",
                created.body
            ),
        }
    }

    // A list naming NO routing action has no type of the request to name, so the refusal
    // states the ones a rule may end with instead.
    let mut fields = path_pattern("/no-routing-action");
    fields.extend(routing_action(1, "authenticate-cognito"));
    let created = create_rule(&router, &listener, "90", &fields).await;
    assert_eq!(
        (created.status, created.error_code()),
        (400, INVALID_ACTION.to_string()),
        "case a list with no routing action: expected the modelled refusal, got {} {}",
        created.status,
        created.body
    );
    assert!(
        message(&created).contains("exactly one of the following actions"),
        "case a list with no routing action: expected the refusal to state the types a rule \
         may end with, got {}",
        created.body
    );
}

/// RULE: a rule read naming neither selector is refused about the two selectors it could
/// have named, and a read naming either one is served.
#[tokio::test]
async fn a_rule_read_naming_neither_selector_is_refused_about_the_selectors() {
    let (router, _engine) = emulator();
    let listener = listener_on(&router, "rule-read-selectors", 80).await;
    let rule = add_rule(
        &router,
        "a rule to select",
        &listener,
        1,
        &[path_pattern("/one"), fixed_response("404")].concat(),
    )
    .await;

    // The rows span what the rule generalises over: which selector the request names, both
    // of them together, and the requests that name neither — alone and beside a member that
    // is no selector.
    let rows: Vec<(&str, Form, bool)> = vec![
        (
            "the listener selector",
            vec![p("ListenerArn", &listener)],
            true,
        ),
        (
            "the rule selector",
            vec![p("RuleArns.member.1", &rule)],
            true,
        ),
        (
            "both selectors, which name the rules of that listener",
            vec![p("ListenerArn", &listener), p("RuleArns.member.1", &rule)],
            true,
        ),
        ("neither selector", Vec::new(), false),
        (
            "neither selector beside a page size",
            vec![p("PageSize", "10")],
            false,
        ),
        (
            "neither selector beside a marker",
            vec![p("Marker", "")],
            false,
        ),
    ];
    for (case, selector, served) in rows {
        let mut form = action("DescribeRules");
        form.extend(selector.iter().cloned());
        let reply = call(&router, &form).await;
        if served {
            assert_eq!(
                reply.status, 200,
                "case {case:?}: expected the read to be served, got {} {}",
                reply.status, reply.body
            );
            continue;
        }
        assert_eq!(
            (reply.status, reply.error_code(), message(&reply)),
            (
                400,
                VALIDATION_ERROR.to_string(),
                rule_selector_required_message()
            ),
            "case {case:?}: expected the refusal about the two selectors, got {} {}",
            reply.status,
            reply.body
        );
    }
}

/// One batch of `(rule ARN, priority)` pairs, as a row of the re-prioritisation table
/// drives it.
type Batch = Vec<(String, i32)>;

/// RULE: a re-prioritisation naming a rule the store does not hold is refused about the
/// rules it named, and every rule it does hold keeps its priority.
#[tokio::test]
async fn a_re_prioritisation_naming_an_absent_rule_is_refused_about_the_rules() {
    let (router, _engine) = emulator();
    let listener = listener_on(&router, "rule-priorities-wording", 80).await;
    let live = add_rule(
        &router,
        "the rule that stays",
        &listener,
        1,
        &[path_pattern("/live"), fixed_response("404")].concat(),
    )
    .await;
    let deleted = add_rule(
        &router,
        "the rule the batch names after it is deleted",
        &listener,
        2,
        &[path_pattern("/deleted"), fixed_response("404")].concat(),
    )
    .await;
    let mut form = action("DeleteRule");
    form.push(p("RuleArn", &deleted));
    let removed = call(&router, &form).await;
    assert_eq!(
        removed.status, 200,
        "setup: the rule must be deleted, got {} {}",
        removed.status, removed.body
    );
    let before = rules_of(&router, "before the batch", &listener).await;

    // The rows span what the rule generalises over: the absent rule alone, paired with a
    // live rule's move, and in either position of the batch.
    let rows: Vec<(&str, Batch)> = vec![
        ("the deleted rule alone", vec![(deleted.clone(), 5)]),
        (
            "the deleted rule paired with a live rule's move",
            vec![(live.clone(), 7), (deleted.clone(), 8)],
        ),
        (
            "the deleted rule named first",
            vec![(deleted.clone(), 8), (live.clone(), 7)],
        ),
    ];
    for (case, pairs) in rows {
        let mut form = action("SetRulePriorities");
        for (index, (rule_arn, priority)) in pairs.iter().enumerate() {
            let position = index + 1;
            form.push(p(
                &format!("RulePriorities.member.{position}.RuleArn"),
                rule_arn,
            ));
            form.push(p(
                &format!("RulePriorities.member.{position}.Priority"),
                &priority.to_string(),
            ));
        }
        let reply = call(&router, &form).await;
        assert_eq!(
            (reply.status, reply.error_code(), message(&reply)),
            (
                400,
                RULE_NOT_FOUND.to_string(),
                rule_selection_not_found_message()
            ),
            "case {case:?}: expected the refusal about the rules the batch named, got {} {}",
            reply.status,
            reply.body
        );
        let after = rules_of(&router, case, &listener).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused batch moves no rule, got {after:?}"
        );
    }

    // And a batch naming only rules the store holds is served, so the refusal above is
    // about the absent rule rather than about the operation.
    let mut form = action("SetRulePriorities");
    form.push(p("RulePriorities.member.1.RuleArn", &live));
    form.push(p("RulePriorities.member.1.Priority", "9"));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case a batch naming only live rules: expected 200, got {} {}",
        reply.status, reply.body
    );
    let after = rules_of(&router, "after the accepted batch", &listener).await;
    assert_ne!(
        after, before,
        "case a batch naming only live rules: expected the move to be applied, got {after:?}"
    );
}
