// SPDX-License-Identifier: CC0-1.0

//! Which `CreateTargetGroup` members a target group may carry for the kind of target it
//! holds, for the one target type whose targets are INVOKED rather than connected to.
//!
//! A `lambda` target group has no network identity: the load balancer invokes the function
//! instead of opening a connection to an address, so nothing about where or how traffic
//! reaches its targets applies to it. The model says so member by member
//! (`smithy_query(query="doc",shape="CreateTargetGroupInput",member="<Member>")`):
//!
//! * `Protocol` — "the protocol to use for routing traffic to the targets … if the target is
//!   a Lambda function, this parameter does not apply";
//! * `Port` — "the port on which the targets receive traffic … if the target is a Lambda
//!   function, this parameter does not apply";
//! * `VpcId` — "the identifier of the virtual private cloud (VPC). If the target is a Lambda
//!   function, this parameter does not apply";
//! * `HealthCheckProtocol` and `HealthCheckPort` — the protocol the load balancer "uses when
//!   performing health checks on targets" and "the port the load balancer uses when
//!   performing health checks on targets": both address the target over the network, which
//!   an invoked function is not reached over, and which is why such a group reports neither.
//!
//! The model declares each member on its own and states no dependency on `TargetType`, so
//! the boundary validator passes the combination and this is the only gate: a request that
//! names one of them for such a target group is self-inconsistent, so it is the request
//! that has to be fixed — this service's `ValidationError` — rather than a value silently
//! dropped into a target group that can never honour it. The rule is read here ONCE, so
//! every operation that settles a target group's network identity refuses the same member
//! set in the same words.

use super::LAMBDA_TARGET_TYPE;

/// Whether a string member a request carried STATES a value.
///
/// An awsQuery request can carry a member with nothing in it; such a member asks for
/// nothing, so it is read exactly as an absent one — it names no value for a rule to be
/// about, and no value to store.
pub fn member_stated(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// How Elastic Load Balancing NAMES an input member in the refusal below: by its own API
/// member name, except where the service writes that name out in prose instead.
///
/// Recorded behaviour, member by member: the refusal about `Port` names it `Port`, while
/// the one about `VpcId` names it `VPC ID`. Only a member the service is recorded spelling
/// differently from its API name has a row here.
pub fn stated_member_name(member: &str) -> &str {
    match member {
        "VpcId" => "VPC ID",
        named => named,
    }
}

/// The message a create that names `member` for a target group of `target_type` — a target
/// type that has no such member — is refused with.
///
/// Recorded behaviour: the sentence is fixed text, and its two variable parts are the
/// offending input member ([`stated_member_name`]) and the target type the request asked
/// for, both taken from the request, because together they are what the caller has to
/// change.
pub fn member_not_for_target_type_message(member: &str, target_type: &str) -> String {
    format!(
        "{} cannot be specified for target groups with target type '{target_type}'",
        stated_member_name(member)
    )
}

/// Whether every member `named` carries is one a target group of `target_type` HAS, and —
/// when it is not — the rule the request broke.
///
/// `named` pairs each member's own API name with whether the request STATED it, in the
/// order the service judges them, so the sentence names the first member of the request
/// that does not apply. A member carrying nothing states nothing, which is what the caller
/// of this rule decides.
///
/// Only the target type whose targets are invoked rather than connected to bounds this
/// member set; every other target type has all of them, so no request for one is judged
/// here.
pub fn check_members_for_target_type(
    target_type: &str,
    named: &[(&str, bool)],
) -> Result<(), String> {
    if target_type != LAMBDA_TARGET_TYPE {
        return Ok(());
    }
    for (member, stated) in named {
        if *stated {
            return Err(member_not_for_target_type_message(member, target_type));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every member of the set, asked of the target type that has none of them and of the
    /// target types that have them all.
    #[test]
    fn the_network_members_belong_to_every_target_type_but_lambda() {
        let members = [
            "Protocol",
            "Port",
            "VpcId",
            "HealthCheckProtocol",
            "HealthCheckPort",
        ];
        for member in members {
            let stated = stated_member_name(member);
            assert_eq!(
                check_members_for_target_type(LAMBDA_TARGET_TYPE, &[(member, true)]),
                Err(format!(
                    "{stated} cannot be specified for target groups with target type 'lambda'"
                )),
                "case {member} on a lambda target group: expected the refusal to name the member"
            );
            // The same member, not stated, is outside the rule.
            assert_eq!(
                check_members_for_target_type(LAMBDA_TARGET_TYPE, &[(member, false)]),
                Ok(()),
                "case {member} left out of a lambda create: expected it to be accepted"
            );
            // And every target type whose targets are reached over the network has it.
            for target_type in ["instance", "ip", "alb"] {
                assert_eq!(
                    check_members_for_target_type(target_type, &[(member, true)]),
                    Ok(()),
                    "case {member} on a {target_type} target group: expected it to be accepted"
                );
            }
        }
    }

    /// The sentence is about the FIRST member of the request that does not apply, in the
    /// order the caller lists them.
    #[test]
    fn the_refusal_names_the_first_member_that_does_not_apply() {
        assert_eq!(
            check_members_for_target_type(
                LAMBDA_TARGET_TYPE,
                &[("Port", true), ("Protocol", true), ("VpcId", true)],
            ),
            Err(member_not_for_target_type_message("Port", LAMBDA_TARGET_TYPE)),
            "case three members at once: expected the refusal to name the first of them"
        );
        assert_eq!(
            check_members_for_target_type(
                LAMBDA_TARGET_TYPE,
                &[("Port", false), ("Protocol", true), ("VpcId", true)],
            ),
            Err(member_not_for_target_type_message(
                "Protocol",
                LAMBDA_TARGET_TYPE
            )),
            "case the first member left out: expected the refusal to name the next one"
        );
    }
}
