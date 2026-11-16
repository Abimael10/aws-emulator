// SPDX-License-Identifier: CC0-1.0

//! Two rules about what a LISTENER may be pointed at, over the wire.
//!
//! * ONE LISTENER PER PORT OF A LOAD BALANCER. A modification that moves a listener onto a
//!   port another listener of the same load balancer holds is refused, and both listeners
//!   are left as they were. Recorded behaviour words that refusal about the `Port` member
//!   the request carried — `ValidationError`, "Listener port '<port>' is already in use" —
//!   because a modification already names the listener it changes, so the port is the one
//!   thing the caller can correct; the CREATE of a second listener on a taken port is the
//!   modelled `DuplicateListener` instead, which `all/aws_error_wording.rs` pins.
//! * ONE LOAD BALANCER PER TARGET GROUP. `TargetGroup$LoadBalancerArns`
//!   (`smithy_query(query="shape",shape="TargetGroup")`) names "the load balancers that
//!   route traffic to this target group", and every operation that attaches a forward
//!   action models `TargetGroupAssociationLimitException` ("you've reached the limit on the
//!   number of load balancers per target group"). So a forward action of one load balancer
//!   naming a target group ANOTHER already routes to is refused, nothing is attached, and
//!   the group's `LoadBalancerArns` keeps the one load balancer it had. A second listener
//!   or rule of the SAME load balancer forwarding there is not at the limit, and a group
//!   nothing routes to any more is free again.
//!
//! The rows differ along what the rules generalise over: the ports a load balancer already
//! holds, which port a modification moves a listener onto, which operation attaches the
//! action (a listener's default actions on the create and on the modification, and a
//! rule's on the create and on the modification), and whether the load balancer attaching
//! it is the one already routing there.

use crate::listener_rules::{
    add_rule, create_listener, create_load_balancer, forward_to, host_header, Form,
};
use crate::provisioning_loop::{action, call, create_http_target_group, emulator, members, p, Reply};
use crate::target_group_attributes_and_modification::unescaped;
use smithy_server_runtime::MultiServiceRouter;

/// The error code this service answers a port a load balancer already listens on a
/// MODIFICATION of with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The awsQuery error code of `TargetGroupAssociationLimitException`
/// (`smithy_query(query="operation",operation="CreateListener")`).
const TARGET_GROUP_ASSOCIATION_LIMIT: &str = "TargetGroupAssociationLimit";

/// The status, code and message of a refusal, as a caller reads them.
fn refusal(reply: &Reply) -> (u16, String, String) {
    (
        reply.status,
        reply.error_code(),
        unescaped(&reply.text("Message").unwrap_or_default()),
    )
}

/// The sentence a modification onto a taken port is refused with: fixed text whose one
/// variable part is the port the request asked for.
fn port_in_use(port: i32) -> String {
    format!("Listener port '{port}' is already in use")
}

/// The sentence an action naming a target group another load balancer routes to is refused
/// with: fixed text, with the target group ARNs the request named.
fn association_limit(target_group_arns: &[&str]) -> String {
    format!(
        "The following target groups cannot be associated with more than one load \
         balancer: {}",
        target_group_arns.join(", ")
    )
}

/// The port one listener reports.
async fn listener_port(router: &MultiServiceRouter, arn: &str) -> Option<String> {
    let mut form = action("DescribeListeners");
    form.push(p("ListenerArns.member.1", arn));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "a read of a listener must be accepted, got {} body {}",
        reply.status, reply.body
    );
    reply.text("Port")
}

/// The load balancers one target group reports as routing to it.
async fn routing_load_balancers(router: &MultiServiceRouter, arn: &str) -> Vec<String> {
    let mut form = action("DescribeTargetGroups");
    form.push(p("TargetGroupArns.member.1", arn));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "a read of a target group must be accepted, got {} body {}",
        reply.status, reply.body
    );
    members(&reply.body, "LoadBalancerArns")
}

#[tokio::test]
async fn modify_listener_onto_a_taken_port_is_refused_naming_the_port() {
    let (router, _engine) = emulator();
    let lb = create_load_balancer(&router, "port-in-use-alb").await;
    let tg = create_http_target_group(&router, "port-in-use-tg").await;

    // Several ports of one load balancer, so the refusal names the port the request asked
    // for rather than one run's.
    let held = [80, 443, 8080];
    let mut listeners = Vec::new();
    for port in held {
        listeners.push((port, create_listener(&router, &lb, &tg, port).await));
    }

    for (port, arn) in &listeners {
        for taken in held.iter().filter(|other| *other != port) {
            let mut form = action("ModifyListener");
            form.push(p("ListenerArn", arn));
            form.push(p("Port", &taken.to_string()));
            let reply = call(&router, &form).await;
            assert_eq!(
                refusal(&reply),
                (400, VALIDATION_ERROR.to_string(), port_in_use(*taken)),
                "case moving {port} onto the occupied port {taken}: unexpected answer, got {}",
                reply.body
            );
            // Both listeners are left as they were.
            assert_eq!(
                listener_port(&router, arn).await.as_deref(),
                Some(port.to_string().as_str()),
                "case moving {port} onto {taken}: a refused modification must change nothing"
            );
        }
        // A port NOTHING holds is accepted, so the refusal is about the collision rather
        // than about moving a listener at all.
        let free = 9000 + port;
        let mut form = action("ModifyListener");
        form.push(p("ListenerArn", arn));
        form.push(p("Port", &free.to_string()));
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "case moving {port} onto the free port {free}: expected 200, got {} body {}",
            reply.status, reply.body
        );
        // Put it back, so the next row still finds every port of `held` taken.
        let mut form = action("ModifyListener");
        form.push(p("ListenerArn", arn));
        form.push(p("Port", &port.to_string()));
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "case restoring {port}: expected 200, got {} body {}",
            reply.status, reply.body
        );
    }
}

#[tokio::test]
async fn a_target_group_another_load_balancer_routes_to_cannot_be_forwarded_to() {
    let (router, _engine) = emulator();
    let first = create_load_balancer(&router, "exclusive-tg-alb-one").await;
    let second = create_load_balancer(&router, "exclusive-tg-alb-two").await;
    let tg = create_http_target_group(&router, "exclusive-tg").await;
    // The second load balancer's own group, so a row that needs an accepted action has one
    // to point at.
    let other_tg = create_http_target_group(&router, "exclusive-tg-other").await;

    // The first load balancer routes to the group.
    let held = create_listener(&router, &first, &tg, 80).await;
    assert_eq!(
        routing_load_balancers(&router, &tg).await,
        vec![first.clone()],
        "the group must report the one load balancer that routes to it"
    );

    // A listener of the second load balancer, which every row below acts on or beside.
    let intruder = create_listener(&router, &second, &other_tg, 80).await;
    let intruder_rule = add_rule(
        &router,
        "the second load balancer's own rule",
        &intruder,
        10,
        &[host_header("rule.example.com"), forward_to(&other_tg)].concat(),
    )
    .await;

    // (case, the operation, the fields that attach the taken group)
    let rows: Vec<(&str, &str, Form)> = vec![
        (
            "a second load balancer's listener created against it",
            "CreateListener",
            [
                vec![
                    p("LoadBalancerArn", &second),
                    p("Protocol", "HTTP"),
                    p("Port", "8080"),
                ],
                default_actions(&tg),
            ]
            .concat(),
        ),
        (
            "a second load balancer's listener modified onto it",
            "ModifyListener",
            [vec![p("ListenerArn", &intruder)], default_actions(&tg)].concat(),
        ),
        (
            "a second load balancer's rule created against it",
            "CreateRule",
            [
                vec![p("ListenerArn", &intruder), p("Priority", "20")],
                host_header("taken.example.com"),
                forward_to(&tg),
            ]
            .concat(),
        ),
        (
            "a second load balancer's rule modified onto it",
            "ModifyRule",
            [vec![p("RuleArn", &intruder_rule)], forward_to(&tg)].concat(),
        ),
    ];

    for (case, operation, fields) in &rows {
        let mut form = action(operation);
        form.extend(fields.iter().cloned());
        let reply = call(&router, &form).await;
        assert_eq!(
            refusal(&reply),
            (
                400,
                TARGET_GROUP_ASSOCIATION_LIMIT.to_string(),
                association_limit(&[&tg])
            ),
            "case {case}: unexpected answer, got {}",
            reply.body
        );
        // Nothing was attached: the group still reports the one load balancer that routes
        // to it.
        assert_eq!(
            routing_load_balancers(&router, &tg).await,
            vec![first.clone()],
            "case {case}: a refused action must leave the association alone"
        );
    }

    // The SAME load balancer may forward there from as many listeners and rules as it
    // likes: the limit counts load balancers, not actions.
    let sibling = create_listener(&router, &first, &tg, 443).await;
    assert_eq!(
        routing_load_balancers(&router, &tg).await,
        vec![first.clone()],
        "a second listener of the same load balancer must not change the association"
    );
    add_rule(
        &router,
        "a rule of the load balancer that already routes there",
        &sibling,
        10,
        &[host_header("same.example.com"), forward_to(&tg)].concat(),
    )
    .await;

    // And once NOTHING routes to the group any more, the identical request succeeds.
    for listener in [&held, &sibling] {
        let mut form = action("DeleteListener");
        form.push(p("ListenerArn", listener));
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "the listener must be deleted, got {} body {}",
            reply.status, reply.body
        );
    }
    assert!(
        routing_load_balancers(&router, &tg).await.is_empty(),
        "with its listeners gone the group must report no load balancer"
    );
    let mut form = action("CreateListener");
    form.extend(
        [
            vec![
                p("LoadBalancerArn", &second),
                p("Protocol", "HTTP"),
                p("Port", "8080"),
            ],
            default_actions(&tg),
        ]
        .concat(),
    );
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "once nothing routes to the group the identical create must succeed, got {} body {}",
        reply.status, reply.body
    );
    assert_eq!(
        routing_load_balancers(&router, &tg).await,
        vec![second.clone()],
        "the group must now report the load balancer that took it"
    );
}

/// A listener's default actions, forwarding to `tg_arn`.
fn default_actions(tg_arn: &str) -> Form {
    vec![
        p("DefaultActions.member.1.Type", "forward"),
        p("DefaultActions.member.1.TargetGroupArn", tg_arn),
    ]
}
