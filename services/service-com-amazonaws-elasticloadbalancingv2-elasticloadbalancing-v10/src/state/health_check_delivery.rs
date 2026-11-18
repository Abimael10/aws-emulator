// SPDX-License-Identifier: CC0-1.0

//! Whether the network a load balancer performs its health checks FROM can carry one to the
//! address a registration names.
//!
//! A health check is a connection a load balancer node opens to the target's own address out
//! of the subnets the load balancer is placed in, so it travels the network those subnets
//! belong to — the one the target group itself names: `CreateTargetGroup$VpcId` is the VPC
//! "in which the targets are located", and a VPC carries traffic to the addresses held INSIDE
//! it. It also carries traffic to a VPC an accepted peering joins it to — "a VPC peering
//! connection is a networking connection between two VPCs that enables you to route traffic
//! between them" (Amazon VPC User Guide), which is why `TargetType`'s own `ip` entry admits an
//! address from a private range OUTSIDE the group's VPC at all ([`super::network`]'s
//! `PRIVATE_TARGET_RANGES`). So the networks a check can reach are the group's own and the
//! ones peered with it, and an address held in any OTHER VPC is one no check of that group
//! ever reaches — and a check that cannot be delivered is a check that fails, exactly as one
//! no security group admits is ([`super::health_check_firewall`]): such a registration is
//! never reported healthy and settles on `unhealthy` once the group's
//! `UnhealthyThresholdCount` consecutive checks have failed
//! ([`super::target_health::TargetProbe`]).
//!
//! Nothing is assumed from silence, the discipline every fact this service does not own is
//! read under ([`super::network`]): a target group carrying no VPC, a VPC id nothing filled
//! in, and an address the account's network interfaces do not hold, leave the question
//! UNDECIDED, and an undecided question fails no check. A peering that is not ACTIVE joins
//! nothing — a request still awaiting acceptance, or one that failed or was deleted, carries
//! no traffic — so it leaves the two networks as separate as no peering at all.
//!
//! Only a registration that names an ADDRESS is read against this rule. An `instance`
//! registration is held to it where it is MADE — `RegisterTargets` refuses an instance of
//! another VPC with the modelled `InvalidTarget` ("the specified target does not exist, is not
//! in the same VPC as the target group"), so no such registration exists to check — and a
//! `lambda` target is reached through no network of the account at all.

/// Whether a check of an address the account holds in `address_networks` would have to LEAVE
/// the networks a check performed in the target group's own VPC can reach,
/// `reachable_networks`.
///
/// `true` only when both halves are known and NONE of the networks holding that address is
/// one of those: one interface of a reachable network holding it is a check that arrives, and
/// an address nothing is known about, or a group whose own network is not known, is a question
/// this answers nothing on.
///
/// An id nothing filled in is NOT a network: a record EC2 holds no VPC for reads as a network
/// that is not known, which is silence, and nothing is assumed from silence — so a blank entry
/// is dropped from either half before they are compared.
pub fn check_crosses_networks(reachable_networks: &[String], address_networks: &[String]) -> bool {
    fn named(ids: &[String]) -> Vec<&str> {
        ids.iter()
            .map(|id| id.trim())
            .filter(|id| !id.is_empty())
            .collect()
    }
    let (reachable, holding) = (named(reachable_networks), named(address_networks));
    if reachable.is_empty() || holding.is_empty() {
        return false;
    }
    !holding
        .iter()
        .any(|holding| reachable.contains(holding))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a check crosses networks only when the networks a check from the target group's
    /// own VPC reaches — that VPC and the ones peered with it — and the networks holding the
    /// address are both known, and none of the latter is one of the former.
    ///
    /// The rows span the group's own network, a network peered with it, another one, an
    /// address two interfaces hold (one of them in a reachable network), a group whose own
    /// network is not known, an address no interface holds, and a blank id on either side —
    /// which names no network at all.
    #[test]
    fn a_check_crosses_networks_only_when_no_network_holding_the_address_is_reachable() {
        let networks =
            |ids: &[&str]| -> Vec<String> { ids.iter().map(|id| (*id).to_string()).collect() };
        let own = "vpc-00000000000000001";
        let peered = "vpc-00000000000000002";
        let other = "vpc-00000000000000003";
        let rows: [(&str, Vec<String>, Vec<String>, bool); 10] = [
            (
                "the address is held in the group's own network",
                networks(&[own]),
                networks(&[own]),
                false,
            ),
            (
                "the address is held in a network peered with the group's",
                networks(&[own, peered]),
                networks(&[peered]),
                false,
            ),
            (
                "the address is held in another network",
                networks(&[own]),
                networks(&[other]),
                true,
            ),
            (
                "the address is held in a network no peering joins to the group's",
                networks(&[own, peered]),
                networks(&[other]),
                true,
            ),
            (
                "one of the networks holding it is reachable",
                networks(&[own]),
                networks(&[other, own]),
                false,
            ),
            (
                "every network holding it is another",
                networks(&[own, peered]),
                networks(&[other, "vpc-00000000000000004"]),
                true,
            ),
            (
                "a target group whose own network is not known",
                Vec::new(),
                networks(&[other]),
                false,
            ),
            (
                "an address no interface of the account holds",
                networks(&[own]),
                Vec::new(),
                false,
            ),
            (
                "the address is held by an interface whose network nothing filled in",
                networks(&[own]),
                networks(&[""]),
                false,
            ),
            (
                "a target group whose own network id is blank",
                networks(&[" "]),
                networks(&[other]),
                false,
            ),
        ];
        for (case, reachable, address_networks, expected) in rows {
            let crosses = check_crosses_networks(&reachable, &address_networks);
            assert_eq!(
                crosses, expected,
                "case {case:?}: expected crosses={expected}, got {crosses} for the reachable \
                 networks {reachable:?} and the networks {address_networks:?}"
            );
        }
    }
}
