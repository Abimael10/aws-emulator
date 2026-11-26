// SPDX-License-Identifier: CC0-1.0

//! The two QUOTAS a listener configuration is bounded by, driven over the wire exactly as a
//! client drives them.
//!
//! Two rules are asserted here, each over a SET of inputs rather than over the one call a
//! recording happens to carry:
//!
//! * one `forward` action distributes among at most the published number of target groups
//!   (`target-groups-per-action-on-application-load-balancer`), so an action naming more is
//!   refused with this API's own `ValidationError` and the listener or rule is not stored —
//!   on every operation that SETS an action, and whichever of the two spellings names the
//!   groups;
//! * one load balancer carries at most the published number of listeners
//!   (`listeners-per-application-load-balancer`), so the one past it is refused with the
//!   modelled `TooManyListeners` naming the load balancer the request addressed.
//!
//! Every refusal is followed by a read proving nothing was stored, and the accepted row at
//! each limit proves the bound is the published number rather than one below it.

use crate::listener_rules::{
    add_rule, create_listener, create_load_balancer, create_rule, fixed_response, rules_of, Form,
};
use crate::provisioning_loop::{action, call, create_http_target_group, emulator, members, p, Reply};
use crate::trust_store_lifecycle::message;
use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::state::{
    forward_target_groups_message, listener_limit_message, LISTENERS_PER_LOAD_BALANCER,
    TARGET_GROUPS_PER_FORWARD_ACTION,
};
use smithy_server_runtime::MultiServiceRouter;

/// The error code this service answers an input its own documented rules refuse with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The awsQuery error code of `TooManyListenersException`
/// (`smithy_query(query="operation",operation="CreateListener")`: `awsQueryError` code
/// `TooManyListeners`, HTTP 400).
const TOO_MANY_LISTENERS: &str = "TooManyListeners";

/// How a row spells the target groups one forward action distributes among.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// `ForwardConfig.TargetGroups.member.N.TargetGroupArn` — the list form.
    Config,
    /// The same groups, each also carrying the `Weight` the model gives the tuple.
    Weighted,
}

/// The `Actions.member.1` fields of one forward action over `groups`.
fn forward(member: &str, spelling: Spelling, groups: &[String]) -> Form {
    let mut form = vec![p(&format!("{member}.member.1.Type"), "forward")];
    for (index, group) in groups.iter().enumerate() {
        let position = index + 1;
        form.push(p(
            &format!("{member}.member.1.ForwardConfig.TargetGroups.member.{position}.TargetGroupArn"),
            group,
        ));
        if spelling == Spelling::Weighted {
            form.push(p(
                &format!("{member}.member.1.ForwardConfig.TargetGroups.member.{position}.Weight"),
                "1",
            ));
        }
    }
    form
}

/// Create `count` HTTP target groups named after `label`, so a row can name as many as the
/// published limit and one more.
async fn target_groups(router: &MultiServiceRouter, label: &str, count: usize) -> Vec<String> {
    let mut groups = Vec::with_capacity(count);
    for index in 0..count {
        groups.push(create_http_target_group(router, &format!("{label}-{index}")).await);
    }
    groups
}

/// Replace the actions of the rule `rule_arn` names.
async fn modify_rule(router: &MultiServiceRouter, rule_arn: &str, fields: &Form) -> Reply {
    let mut form = action("ModifyRule");
    form.push(p("RuleArn", rule_arn));
    form.extend(fields.iter().cloned());
    call(router, &form).await
}

/// Replace the default actions of the listener `listener_arn` names.
async fn modify_listener(
    router: &MultiServiceRouter,
    listener_arn: &str,
    fields: &Form,
) -> Reply {
    let mut form = action("ModifyListener");
    form.push(p("ListenerArn", listener_arn));
    form.extend(fields.iter().cloned());
    call(router, &form).await
}

/// The listeners one load balancer reports, by port.
async fn listener_ports(router: &MultiServiceRouter, load_balancer_arn: &str) -> Vec<String> {
    let mut form = action("DescribeListeners");
    form.push(p("LoadBalancerArn", load_balancer_arn));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "the listeners must be readable back, got {} {}",
        reply.status, reply.body
    );
    members(&reply.body, "Listeners")
        .iter()
        .filter_map(|listener| crate::provisioning_loop::first_text(listener, "Port"))
        .collect()
}

/// RULE: a forward action names at most the published number of target groups, on every
/// operation that sets an action.
#[tokio::test]
async fn a_forward_action_names_at_most_the_published_number_of_target_groups() {
    let (router, _engine) = emulator();
    let load_balancer = create_load_balancer(&router, "forward-action-limit").await;
    // One group more than the limit, so a row can name the limit, one past it, and fewer.
    let groups = target_groups(
        &router,
        "forward-action-limit",
        TARGET_GROUPS_PER_FORWARD_ACTION + 1,
    )
    .await;
    let listener = create_listener(&router, &load_balancer, &groups[0], 80).await;
    let existing = add_rule(
        &router,
        "the rule a modification replaces the actions of",
        &listener,
        1,
        &[
            vec![
                p("Conditions.member.1.Field", "path-pattern"),
                p("Conditions.member.1.PathPatternConfig.Values.member.1", "/keep"),
            ],
            fixed_response("404"),
        ]
        .concat(),
    )
    .await;

    // The rows span what the rule generalises over: how many groups the action names (one,
    // the published limit, one past it, well past it) and whether the tuples carry weights.
    let rows: Vec<(String, usize, Spelling)> = vec![
        ("one target group".to_string(), 1, Spelling::Config),
        (
            format!("the published limit of {TARGET_GROUPS_PER_FORWARD_ACTION}"),
            TARGET_GROUPS_PER_FORWARD_ACTION,
            Spelling::Config,
        ),
        (
            "the published limit, each group weighted".to_string(),
            TARGET_GROUPS_PER_FORWARD_ACTION,
            Spelling::Weighted,
        ),
        (
            "one past the published limit".to_string(),
            TARGET_GROUPS_PER_FORWARD_ACTION + 1,
            Spelling::Config,
        ),
        (
            "one past the published limit, each group weighted".to_string(),
            TARGET_GROUPS_PER_FORWARD_ACTION + 1,
            Spelling::Weighted,
        ),
    ];

    for (index, (case, count, spelling)) in rows.iter().enumerate() {
        let named = &groups[..*count];
        let accepted = *count <= TARGET_GROUPS_PER_FORWARD_ACTION;
        let port = i32::try_from(index + 100).expect("a listener port");
        let priority = i32::try_from(index + 10).expect("a rule priority");

        // CreateListener, whose default actions this action is.
        let mut form = action("CreateListener");
        form.push(p("LoadBalancerArn", &load_balancer));
        form.push(p("Protocol", "HTTP"));
        form.push(p("Port", &port.to_string()));
        form.extend(forward("DefaultActions", *spelling, named));
        let created_listener = call(&router, &form).await;
        if accepted {
            assert_eq!(
                created_listener.status, 200,
                "case {case:?} on CreateListener: expected 200, got {} {}",
                created_listener.status, created_listener.body
            );
        } else {
            assert_eq!(
                (
                    created_listener.status,
                    created_listener.error_code(),
                    message(&created_listener)
                ),
                (
                    400,
                    VALIDATION_ERROR.to_string(),
                    forward_target_groups_message()
                ),
                "case {case:?} on CreateListener: expected the refusal the published limit \
                 earns, got {} {}",
                created_listener.status,
                created_listener.body
            );
            assert!(
                !listener_ports(&router, &load_balancer)
                    .await
                    .contains(&port.to_string()),
                "case {case:?} on CreateListener: a refused create stores no listener"
            );
        }

        // CreateRule, whose actions it is.
        let before = rules_of(&router, case, &listener).await;
        let fields = [
            vec![
                p("Conditions.member.1.Field", "path-pattern"),
                p(
                    "Conditions.member.1.PathPatternConfig.Values.member.1",
                    &format!("/row-{index}"),
                ),
            ],
            forward("Actions", *spelling, named),
        ]
        .concat();
        let created_rule = create_rule(&router, &listener, &priority.to_string(), &fields).await;
        if accepted {
            assert_eq!(
                created_rule.status, 200,
                "case {case:?} on CreateRule: expected 200, got {} {}",
                created_rule.status, created_rule.body
            );
        } else {
            assert_eq!(
                (
                    created_rule.status,
                    created_rule.error_code(),
                    message(&created_rule)
                ),
                (
                    400,
                    VALIDATION_ERROR.to_string(),
                    forward_target_groups_message()
                ),
                "case {case:?} on CreateRule: expected the refusal the published limit earns, \
                 got {} {}",
                created_rule.status,
                created_rule.body
            );
            let after = rules_of(&router, case, &listener).await;
            assert_eq!(
                after, before,
                "case {case:?} on CreateRule: a refused create stores nothing, got {after:?}"
            );
        }

        // And the two modifications that REPLACE a stored action list.
        let modified_rule = modify_rule(&router, &existing, &forward("Actions", *spelling, named)).await;
        assert_eq!(
            modified_rule.status == 200,
            accepted,
            "case {case:?} on ModifyRule: expected accepted={accepted}, got {} {}",
            modified_rule.status,
            modified_rule.body
        );
        if !accepted {
            assert_eq!(
                (modified_rule.error_code(), message(&modified_rule)),
                (
                    VALIDATION_ERROR.to_string(),
                    forward_target_groups_message()
                ),
                "case {case:?} on ModifyRule: expected the refusal the published limit earns, \
                 got {}",
                modified_rule.body
            );
        }
        let modified_listener =
            modify_listener(&router, &listener, &forward("DefaultActions", *spelling, named)).await;
        assert_eq!(
            modified_listener.status == 200,
            accepted,
            "case {case:?} on ModifyListener: expected accepted={accepted}, got {} {}",
            modified_listener.status,
            modified_listener.body
        );
        if !accepted {
            assert_eq!(
                (modified_listener.error_code(), message(&modified_listener)),
                (
                    VALIDATION_ERROR.to_string(),
                    forward_target_groups_message()
                ),
                "case {case:?} on ModifyListener: expected the refusal the published limit \
                 earns, got {}",
                modified_listener.body
            );
        }
    }
}

/// RULE: one load balancer carries at most the published number of listeners, and the one
/// past it is refused naming the load balancer the request addressed.
#[tokio::test]
async fn a_load_balancer_carries_at_most_the_published_number_of_listeners() {
    let (router, _engine) = emulator();
    let load_balancer = create_load_balancer(&router, "listener-limit").await;
    let group = create_http_target_group(&router, "listener-limit-tg").await;

    // Every listener up to the published limit is accepted, each on a port of its own.
    for index in 0..LISTENERS_PER_LOAD_BALANCER {
        let port = i32::try_from(index + 1).expect("a listener port");
        let mut form = action("CreateListener");
        form.push(p("LoadBalancerArn", &load_balancer));
        form.push(p("Protocol", "HTTP"));
        form.push(p("Port", &port.to_string()));
        form.push(p("DefaultActions.member.1.Type", "forward"));
        form.push(p("DefaultActions.member.1.TargetGroupArn", &group));
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "case listener {} of {LISTENERS_PER_LOAD_BALANCER}: expected 200, got {} {}",
            index + 1,
            reply.status,
            reply.body
        );
    }
    let filled = listener_ports(&router, &load_balancer).await;
    assert_eq!(
        filled.len(),
        LISTENERS_PER_LOAD_BALANCER,
        "the load balancer must carry the published number of listeners, got {filled:?}"
    );

    // The rows past the limit span what the rule generalises over: the port the request asks
    // for. Each is refused the same way, and none of them is stored.
    let rows: &[(&str, &str, i32)] = &[
        ("one past the limit", "HTTP", 60_001),
        ("another port past the limit", "HTTP", 60_002),
        ("the highest port, past the limit", "HTTP", 65_535),
    ];
    for (case, protocol, port) in rows {
        let mut form = action("CreateListener");
        form.push(p("LoadBalancerArn", &load_balancer));
        form.push(p("Protocol", protocol));
        form.push(p("Port", &port.to_string()));
        form.push(p("DefaultActions.member.1.Type", "forward"));
        form.push(p("DefaultActions.member.1.TargetGroupArn", &group));
        let reply = call(&router, &form).await;
        assert_eq!(
            (reply.status, reply.error_code(), message(&reply)),
            (
                400,
                TOO_MANY_LISTENERS.to_string(),
                listener_limit_message(&load_balancer)
            ),
            "case {case:?}: expected the modelled quota refusal naming the load balancer, got \
             {} {}",
            reply.status,
            reply.body
        );
        let after = listener_ports(&router, &load_balancer).await;
        assert_eq!(
            after, filled,
            "case {case:?}: a refused create stores no listener, got {after:?}"
        );
    }
}
