// SPDX-License-Identifier: CC0-1.0

//! Whether a target of a `lambda` target group may name a `Port`, on every operation that
//! takes a `Targets` list.
//!
//! RULE: `TargetDescription$Port` is "the port on which the target is listening … this
//! parameter is not used if the target is a Lambda function"
//! (`smithy_query(query="doc",shape="TargetDescription",member="Port")`), so a target of a
//! target group whose `TargetType` is `lambda` names no port — and a request that names one
//! anyway is refused with `ValidationError` (400) and the ENTIRELY FIXED sentence
//! [`PORT_NOT_FOR_LAMBDA_TARGET_MESSAGE`], with nothing stored, changed or reported for it.
//! The verdict is on the MEMBER rather than on what is being asked of it, so the
//! registration that adds a target, the deregistration that removes one and the health read
//! that reports one are refused alike; every other target type HAS the member, so for those
//! the same calls are served.
//!
//! The rows span what the rule generalises over: the port value (each published bound of the
//! modelled range `1..65535`, the two protocol ports a client is most likely to send and the
//! one the model singles out for GENEVE), the operation carrying it, the position of the
//! offending target in a list of several, and the target type of the group — against the
//! same calls with no `Port`, which must succeed.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::state::PORT_NOT_FOR_LAMBDA_TARGET_MESSAGE;

use crate::provisioning_loop::{account, action, call, emulator, members, p, Reply, REGION};
use crate::target_group_attributes_and_modification::unescaped;
use crate::target_id_forms_and_reported_ports::{one_target, reported_for, target_group_on};

/// The code this API answers an input it cannot accept with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The three operations that take a `Targets` list, which the rule reaches alike.
const OPERATIONS: [&str; 3] = [
    "RegisterTargets",
    "DeregisterTargets",
    "DescribeTargetHealth",
];

/// The ports the rule is asked of: each published bound of the port range the model declares
/// (`smithy_query(query="shape",shape="TargetDescription")`: `Port` carries `range 1..65535`),
/// the two protocol ports a client is most likely to send, and the port the member's own
/// documentation singles out for a GENEVE target group.
const PORTS: [&str; 5] = ["1", "80", "443", "6081", "65535"];

/// A Lambda function ARN of this account, in [`REGION`] — what a target of a `lambda` target
/// group is written as.
fn function(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{}:function:{name}", account())
}

/// The status, code and message of a refusal, as a caller reads them.
fn refusal(reply: &Reply) -> (u16, String, String) {
    (
        reply.status,
        reply.error_code(),
        unescaped(&reply.text("Message").unwrap_or_default()),
    )
}

/// Assert that `reply` is the refusal the rule states, and that it quotes nothing of the
/// request.
fn assert_port_refusal(case: &str, reply: &Reply, quoted: &[&str]) {
    assert_eq!(
        refusal(reply),
        (
            400,
            VALIDATION_ERROR.to_string(),
            PORT_NOT_FOR_LAMBDA_TARGET_MESSAGE.to_string()
        ),
        "case {case:?}: expected 400 {VALIDATION_ERROR} \
         {PORT_NOT_FOR_LAMBDA_TARGET_MESSAGE:?}, got {} {}",
        reply.status,
        reply.body
    );
    let stated = unescaped(&reply.text("Message").unwrap_or_default());
    for value in quoted {
        assert!(
            !stated.contains(value),
            "case {case:?}: the sentence states which member does not apply and quotes \
             nothing of the request, got {stated:?} carrying {value:?}"
        );
    }
}

/// One `Targets` member as a request writes it: the target's id, and the `Port` it names (or
/// `None` for a target that names none).
type NamedTarget = (String, Option<&'static str>);

/// Drive `operation` with a `Targets` list of `(id, port)` pairs.
async fn targets(
    router: &MultiServiceRouter,
    operation: &str,
    arn: &str,
    list: &[NamedTarget],
) -> Reply {
    let mut form = action(operation);
    form.push(p("TargetGroupArn", arn));
    for (index, (id, port)) in list.iter().enumerate() {
        let at = index + 1;
        form.push(p(&format!("Targets.member.{at}.Id"), id));
        if let Some(port) = port {
            form.push(p(&format!("Targets.member.{at}.Port"), port));
        }
    }
    call(router, &form).await
}

/// Every target id the group at `arn` holds a registration for, in the order it reports them.
async fn held(router: &MultiServiceRouter, arn: &str) -> Vec<Option<String>> {
    reported_for(router, arn, &[])
        .await
        .into_iter()
        .map(|reported| reported.id)
        .collect()
}

#[tokio::test]
async fn a_lambda_target_naming_a_port_is_refused_by_every_targets_taking_operation() {
    let (router, _engine) = emulator();

    for (index, port) in PORTS.iter().enumerate() {
        for (at, operation) in OPERATIONS.iter().enumerate() {
            let case = format!("{operation} with Port {port}");
            // A FRESH group per row and operation, so no row is answered for a group another
            // row filled: a `lambda` target group holds one target. The name is short because
            // the service bounds a target group name at 32 characters.
            let arn = target_group_on(
                &router,
                &case,
                &format!("lambda-port-{index}-{at}"),
                "lambda",
                // A Lambda function is invoked rather than connected to, so the group names
                // neither a protocol nor a port of its own.
                None,
                None,
            )
            .await;
            let id = function("greeter");

            let reply = one_target(&router, operation, &arn, &id, Some(port)).await;
            assert_port_refusal(&case, &reply, &[port, &id]);

            // Nothing was stored, removed or reported for the refused call: the group is
            // still empty, and the same call with no `Port` is served.
            assert!(
                held(&router, &arn).await.is_empty(),
                "case {case:?}: a refused call must leave the group exactly as it was"
            );
            let served = one_target(&router, operation, &arn, &id, None).await;
            assert_eq!(
                served.status, 200,
                "case {case:?}: the identical call with no Port must be served, got {} {}",
                served.status, served.body
            );
        }
    }
}

#[tokio::test]
async fn a_port_on_any_target_of_the_list_refuses_the_whole_call() {
    // The whole list is judged before anything is done with any of it, so where in the list
    // the offending target sits does not change the verdict — asked of the two list-taking
    // operations a `lambda` group's one-target quota leaves room for.
    let (router, _engine) = emulator();
    let first = function("greeter");
    let second = function("responder");

    // (case, the list the request carries)
    let lists: [(&str, Vec<NamedTarget>); 3] = [
        (
            "the first of two targets names a port",
            vec![(first.clone(), Some("80")), (second.clone(), None)],
        ),
        (
            "the second of two targets names a port",
            vec![(first.clone(), None), (second.clone(), Some("8080"))],
        ),
        (
            "every target names a port",
            vec![(first.clone(), Some("80")), (second.clone(), Some("443"))],
        ),
    ];

    for (index, (case, list)) in lists.iter().enumerate() {
        for (at, operation) in ["DeregisterTargets", "DescribeTargetHealth"]
            .into_iter()
            .enumerate()
        {
            let case = format!("{case} on {operation}");
            let arn = target_group_on(
                &router,
                &case,
                &format!("lambda-port-list-{index}-{at}"),
                "lambda",
                None,
                None,
            )
            .await;
            // One registered target, so the refusal below cannot be read as "the group holds
            // nothing": it is still registered afterwards.
            let registered = one_target(&router, "RegisterTargets", &arn, &first, None).await;
            assert_eq!(
                registered.status, 200,
                "case {case:?}: the target with no port must register, got {} {}",
                registered.status, registered.body
            );

            let reply = targets(&router, operation, &arn, list).await;
            assert_port_refusal(&case, &reply, &[&first, &second]);
            assert_eq!(
                held(&router, &arn).await,
                vec![Some(first.clone())],
                "case {case:?}: a refused call must leave the group exactly as it was"
            );
        }
    }
}

#[tokio::test]
async fn a_lambda_target_with_no_port_round_trips_through_the_group() {
    // The member is what the rule is about: with it left out the registration is stored, read
    // back through the health read — carrying no port, because a Lambda target has none — and
    // removed again.
    let (router, _engine) = emulator();
    let arn = target_group_on(&router, "a lambda round trip", "lambda-port-ok", "lambda", None, None)
        .await;
    let id = function("greeter");

    let registered = one_target(&router, "RegisterTargets", &arn, &id, None).await;
    assert_eq!(
        registered.status, 200,
        "a lambda target with no port must register, got {} {}",
        registered.status, registered.body
    );

    let reported = reported_for(&router, &arn, &[(id.clone(), None)]).await;
    assert_eq!(
        reported
            .iter()
            .map(|target| (target.id.clone(), target.port.clone()))
            .collect::<Vec<(Option<String>, Option<String>)>>(),
        vec![(Some(id.clone()), None)],
        "the group must report the registration with no port of its own, got {reported:?}"
    );

    let deregistered = one_target(&router, "DeregisterTargets", &arn, &id, None).await;
    assert_eq!(
        deregistered.status, 200,
        "the same target must be deregistered, got {} {}",
        deregistered.status, deregistered.body
    );
}

#[tokio::test]
async fn every_other_target_type_takes_the_port_member() {
    // The member applies to every target type but `lambda` — "the port on which the target is
    // listening" — so the same request carrying the same `Port` is SERVED for a group whose
    // targets are listening sockets, and the port it names is what the group then reports.
    let (router, _engine) = emulator();

    // (case, the group's TargetType, the target id, the port the request names)
    let served: [(&str, &str, &str, &str); 4] = [
        ("an instance target on a port of its own", "instance", "i-0123456789abcdef0", "8080"),
        ("an instance target at the low bound of the port range", "instance", "i-00000000000000011", "1"),
        ("an address target on a port of its own", "ip", "10.0.0.7", "9090"),
        ("an address target at the high bound of the port range", "ip", "10.0.0.8", "65535"),
    ];

    for (index, (case, target_type, id, port)) in served.iter().enumerate() {
        let arn = target_group_on(
            &router,
            case,
            &format!("port-member-{index}"),
            target_type,
            Some("80"),
            None,
        )
        .await;
        let reply = one_target(&router, "RegisterTargets", &arn, id, Some(port)).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: a port is a member of a {target_type} target, got {} {}",
            reply.status, reply.body
        );

        let reported = reported_for(&router, &arn, &[((*id).to_string(), Some(port))]).await;
        assert_eq!(
            reported
                .iter()
                .map(|target| (target.id.clone(), target.port.clone()))
                .collect::<Vec<(Option<String>, Option<String>)>>(),
            vec![(Some((*id).to_string()), Some((*port).to_string()))],
            "case {case:?}: the group must report the registration on the port the request \
             named, got {reported:?}"
        );

        // And the health read and the deregistration take the same member.
        let deregistered = one_target(&router, "DeregisterTargets", &arn, id, Some(port)).await;
        assert_eq!(
            deregistered.status, 200,
            "case {case:?}: the deregistration must take the same port, got {} {}",
            deregistered.status, deregistered.body
        );
    }
}

/// An unfiltered health read of a `lambda` group reports the registration it holds, and that
/// row carries no port at all: the member is not one such a target has.
#[tokio::test]
async fn an_unfiltered_health_read_of_a_lambda_group_names_no_port_at_all() {
    let (router, _engine) = emulator();
    let arn = target_group_on(
        &router,
        "an unfiltered lambda read",
        "lambda-port-unfiltered",
        "lambda",
        None,
        None,
    )
    .await;
    let id = function("greeter");
    let registered = one_target(&router, "RegisterTargets", &arn, &id, None).await;
    assert_eq!(
        registered.status, 200,
        "the registration must be served, got {} {}",
        registered.status, registered.body
    );

    let mut form = action("DescribeTargetHealth");
    form.push(p("TargetGroupArn", &arn));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "the unfiltered read must be served, got {} {}",
        reply.status, reply.body
    );
    let rows = members(&reply.body, "TargetHealthDescriptions");
    assert_eq!(
        rows.len(),
        1,
        "the group must report its one registration, got {}",
        reply.body
    );
    assert!(
        !rows[0].contains("<Port>"),
        "a Lambda target is invoked rather than connected to, so the row carries no port, \
         got {:?}",
        rows[0]
    );
}
