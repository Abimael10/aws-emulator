// SPDX-License-Identifier: CC0-1.0

//! The ORDER this service lists an enum's value set in when it refuses a value outside it
//! — over the wire, through the router.
//!
//! The rule: a constraint clause about an enum member states which values the enum admits
//! (`Member must satisfy enum value set: [<values>]`), and the ORDER those values are
//! listed in is the SERVICE's rather than the model's — it is neither the model's
//! declaration order nor a sort of them, so it cannot be derived from the model. An enum
//! this service publishes an order for is stated in that order whichever member, operation
//! or value tripped the constraint; every other part of the refusal — the status, the
//! `ValidationError` code, the envelope, the member path and the echoed value — stays
//! exactly as the boundary validator states it.
//!
//! The rows differ along what the rule generalises over: the rejected value (several
//! spellings outside the set, including one that differs only in case), the operation that
//! carried it, and the enum (one this service publishes an order for, and one it does not,
//! which keeps the order the model declares).

use crate::provisioning_loop::{action, call, emulator, p};
use crate::target_group_attributes_and_modification::unescaped;

/// The awsQuery error code every refusal below carries.
const VALIDATION_ERROR: &str = "ValidationError";

/// The value set `TargetTypeEnum` is published with, in the order this service lists it.
///
/// The model declares the same four values in another order
/// (`smithy_query(query="shape",shape="TargetTypeEnum")` lists `instance`, `ip`, `lambda`,
/// `alb`), so a clause rendered from the model alone would read differently.
const TARGET_TYPE_SET: &str = "[lambda, ip, instance, alb]";

/// The clause one rejected enum value earns, in the envelope AWS states a single
/// constraint violation in.
fn expected(value: &str, path: &str, set: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy constraint: \
         Member must satisfy enum value set: {set}"
    )
}

#[tokio::test]
async fn the_target_type_value_set_is_stated_in_the_order_this_service_publishes() {
    let (router, _engine) = emulator();

    // (case, the rejected TargetType)
    //
    // Every row is outside the enum: a value differing only in case, one differing in
    // case for another member of the set, a plausible synonym, and a value from another
    // API. What they share is the clause they earn, which states the whole value set.
    let rows: [(&str, &str); 5] = [
        ("the set's own value in upper case", "IP"),
        ("another of the set's values in upper case", "INSTANCE"),
        ("a value in mixed case", "Lambda"),
        ("a plausible synonym", "ec2"),
        ("a value of another API", "ip_address"),
    ];

    for (case, target_type) in rows {
        let mut form = action("CreateTargetGroup");
        form.push(p("Name", "enum-order-tg"));
        form.push(p("Protocol", "HTTP"));
        form.push(p("Port", "80"));
        form.push(p("VpcId", "vpc-12345678"));
        form.push(p("TargetType", target_type));
        let reply = call(&router, &form).await;
        let stated = reply.text("Message").map(|text| unescaped(&text));
        let want = expected(target_type, "targetType", TARGET_TYPE_SET);
        assert_eq!(
            (reply.status, reply.error_code(), stated.clone()),
            (400, VALIDATION_ERROR.to_string(), Some(want.clone())),
            "case {case:?}: expected 400 {VALIDATION_ERROR} {want:?}, got {} body {}",
            reply.status,
            reply.body
        );
    }

    // And the refused create wrote nothing: the target group is not there to be read.
    let mut form = action("DescribeTargetGroups");
    form.push(p("Names.member.1", "enum-order-tg"));
    let after = call(&router, &form).await;
    assert_eq!(
        (after.status, after.error_code().as_str()),
        (400, "TargetGroupNotFound"),
        "case after the refusals: nothing may have been created, got {} body {}",
        after.status,
        after.body
    );
}

#[tokio::test]
async fn an_enum_this_service_publishes_no_order_for_keeps_the_models_order() {
    let (router, _engine) = emulator();

    // `LoadBalancerSchemeEnum`'s values, in the order the model declares them
    // (`smithy_query(query="shape",shape="LoadBalancerSchemeEnum")`): this service
    // publishes no other order for them, so the clause states the one the emulator has a
    // source for.
    let mut form = action("CreateLoadBalancer");
    form.push(p("Name", "enum-order-lb"));
    form.push(p("Subnets.member.1", "subnet-11111111"));
    form.push(p("Subnets.member.2", "subnet-22222222"));
    form.push(p("Scheme", "INTERNAL"));
    let reply = call(&router, &form).await;
    let stated = reply.text("Message").map(|text| unescaped(&text));
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (400, VALIDATION_ERROR),
        "case an unpublished enum: expected 400 {VALIDATION_ERROR}, got {} body {}",
        reply.status,
        reply.body
    );
    let stated = stated.unwrap_or_else(|| panic!("case an unpublished enum: no message in {}", reply.body));
    assert!(
        stated.starts_with(
            "1 validation error detected: Value 'INTERNAL' at 'scheme' failed to satisfy \
             constraint: Member must satisfy enum value set: ["
        ),
        "case an unpublished enum: expected the boundary validator's own clause, got {stated:?}"
    );
    assert!(
        stated.contains("internet-facing") && stated.contains("internal"),
        "case an unpublished enum: expected the model's values in the clause, got {stated:?}"
    );
}
