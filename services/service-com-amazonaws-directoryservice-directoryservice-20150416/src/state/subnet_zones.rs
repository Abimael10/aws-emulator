// SPDX-License-Identifier: CC0-1.0

//! THE PLACEMENT RULES, decided over what the service that owns subnets answered.
//!
//! `DirectoryVpcSettings`' own documentation states them: `VpcId` is "the identifier of the
//! VPC in which to create the directory" and `SubnetIds` "the identifiers of the subnets for
//! the directory servers. The two subnets must be in different Availability Zones. Directory
//! Service creates a directory server and a DNS server in each of these subnets."
//! `DirectoryConnectSettings$SubnetIds` places an AD Connector's servers the same way. The
//! rules therefore have two halves, and they are decided in two different places:
//!
//! * the half that is a fact about the REQUEST — two DISTINCT subnet ids, and no more of
//!   them than a placement may name — is [`super::subnet_pair_defect`], which every
//!   placement runs before anything is resolved or stored;
//! * the halves that are facts about the account's NETWORK — whether those subnets sit in
//!   the VPC the placement names, and whether they sit in different zones — are
//!   [`placement_defect`] below, decided over the subnets
//!   [`super::ec2_subnets::resolve_subnet_networks`] already asked Amazon EC2 about
//!   for the same create. No hop of its own: a create resolves its placement ONCE, and that
//!   one answer serves both refusals and the record the create stores.
//!
//! Nothing here reaches another service, holds a lock or keeps state: it is the decision
//! alone, so what refuses a placement is only ever what EC2 reported.

use std::collections::BTreeMap;

use super::directory_network::SubnetNetwork;

/// Why the subnets a placement names cannot carry a directory's servers, given what EC2
/// reported for them, or `None` when they can.
///
/// The refusal this grounds is the resource-semantics one (`ClientException`), a different
/// class from the validation refusal a MALFORMED subnet id gets and from the
/// `InvalidParameterException` the request-level half ([`super::subnet_pair_defect`])
/// answers.
///
/// TWO facts about the account's network decide it, and both are read off the SAME answer a
/// create already asked EC2 for: that every subnet sits in the VPC the placement names
/// ([`foreign_vpc_defect`]) and that the subnets are not all in one Availability Zone
/// ([`single_zone_defect`]). The membership is stated first, because a subnet of another VPC
/// is not a placement in this VPC at all whatever zone it happens to be in.
///
/// REFUSES ONLY WHAT EC2 GROUNDED, as both halves do: a subnet EC2 reported nothing for, or
/// reported without the fact a half needs, leaves that half undecided, since this service
/// keeps no network of its own to decide it with.
#[must_use]
pub fn placement_defect(
    resolved: &BTreeMap<String, SubnetNetwork>,
    subnet_ids: &[String],
    vpc_id: &str,
) -> Option<String> {
    foreign_vpc_defect(resolved, subnet_ids, vpc_id).or_else(|| single_zone_defect(resolved, subnet_ids))
}

/// Why the subnets a placement names cannot carry the servers of a directory created in
/// `vpc_id` — that EC2 reported one of them in ANOTHER VPC — or `None` when none of them is.
///
/// `DirectoryVpcSettings`' own documentation puts the two members together: `VpcId` is "the
/// identifier of the VPC in which to create the directory" and `SubnetIds` "the identifiers
/// of the subnets for the directory servers", which are the subnets of that VPC. A placement
/// naming a subnet of a different VPC therefore names a network the directory is not being
/// created in, and the servers it asks for could not be placed there.
///
/// Decided over the subnets [`super::ec2_subnets::resolve_subnet_networks`] already asked
/// Amazon EC2 about for the same create, and only over what EC2 GROUNDED: a subnet it
/// reported nothing for, or reported without a VPC, says nothing about where it sits and
/// refuses nothing.
#[must_use]
fn foreign_vpc_defect(
    resolved: &BTreeMap<String, SubnetNetwork>,
    subnet_ids: &[String],
    vpc_id: &str,
) -> Option<String> {
    let elsewhere = subnet_ids.iter().any(|subnet_id| {
        resolved
            .get(subnet_id)
            .and_then(|subnet| subnet.vpc_id.as_deref())
            .is_some_and(|held| held != vpc_id)
    });
    elsewhere.then(|| SUBNETS_OUTSIDE_THE_VPC.to_string())
}

/// Why the subnets a placement names cannot carry a directory's servers, given what EC2
/// reported for them, or `None` when they can.
///
/// REFUSES ONLY WHAT EC2 GROUNDED: the placement is refused when EC2 reported a zone for
/// EVERY subnet the request named and all of those zones are the SAME. A subnet EC2 reported
/// nothing for, or reported without a zone, leaves the topology unknown — and an unknown
/// topology refuses nothing, since this service keeps no zone of its own to decide it with,
/// which is the reading `super::directory_network` states for that hop (it gates no create
/// on silence) and the one `services/common/src/network.rs` assigns to a service that only
/// GATES on what the owner says.
#[must_use]
fn single_zone_defect(
    resolved: &BTreeMap<String, SubnetNetwork>,
    subnet_ids: &[String],
) -> Option<String> {
    let mut zones: Vec<&str> = Vec::new();
    for subnet_id in subnet_ids {
        let zone = resolved
            .get(subnet_id)
            .and_then(|subnet| subnet.availability_zone.as_deref())?;
        if !zones.contains(&zone) {
            zones.push(zone);
        }
    }
    // ONE zone across every subnet the request named — and at least one resolved, so a
    // placement that named nothing says nothing.
    (zones.len() == 1).then(|| SUBNETS_IN_ONE_ZONE.to_string())
}

/// The diagnostic Directory Service refuses a placement whose subnets all sit in ONE
/// Availability Zone with.
///
/// FIXED text, the rule the topology broke: it quotes neither the subnets nor the zone, so
/// every caller that breaks it reads the same sentence. It is a DIFFERENT sentence from
/// [`super::INVALID_SUBNET_IDS`], the one a settings block that names no two distinct subnets
/// gets, and a different error class too — the two refusals are recorded apart, so a client
/// that reads the sentence can tell a pick it must re-zone from a pick that names no pair at
/// all. [`super::refusal`] appends the request id, once.
pub const SUBNETS_IN_ONE_ZONE: &str =
    "Invalid subnet ID(s). The two subnets must be in different Availability Zones.";

/// The diagnostic Directory Service refuses a placement whose subnets do not all sit in the
/// VPC it names with: the invalid-subnets opener this service's recorded placement refusals
/// share, and then the rule the check applies — that both of a placement's subnets belong to
/// the VPC the request named.
///
/// FIXED text, like its two siblings — [`SUBNETS_IN_ONE_ZONE`] for a pick in one zone and
/// [`super::INVALID_SUBNET_IDS`] for a settings block naming no two distinct subnets — so a
/// caller that breaks any of the three placement rules reads one sentence naming its subnet
/// pick as the invalid part of the request and the rule a servable pick satisfies. It quotes
/// neither the subnets nor either VPC. [`super::refusal`] appends the request id, once.
pub const SUBNETS_OUTSIDE_THE_VPC: &str =
    "Invalid subnet ID(s). Both subnets must belong to the specified VPC.";
