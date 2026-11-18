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
//! * the halves that are facts about the account's NETWORK — whether the account HAS a subnet
//!   for each identifier, whether those subnets sit in the VPC the placement names, and
//!   whether they sit in different zones — are
//!   [`placement_defect`] below, decided over the subnets
//!   [`super::ec2_subnets::resolve_subnet_networks`] already asked Amazon EC2 about
//!   for the same create. No hop of its own: a create resolves its placement ONCE, and that
//!   one answer serves both refusals and the record the create stores.
//!
//! Nothing here reaches another service, holds a lock or keeps state: it is the decision
//! alone, so what refuses a placement is only ever what EC2 answered.

use std::collections::BTreeMap;

use super::directory_network::SubnetNetwork;
use super::ec2_subnets::SubnetResolution;

/// Why the subnets a placement names cannot carry a directory's servers, given what EC2
/// answered about them, or `None` when they can.
///
/// The refusal this grounds is the resource-semantics one (`ClientException`), a different
/// class from the validation refusal a MALFORMED subnet id gets and from the
/// `InvalidParameterException` the request-level half ([`super::subnet_pair_defect`])
/// answers.
///
/// THREE facts about the account's network decide it, and all of them are read off the SAME
/// answer a create already asked EC2 for: that the account HAS a subnet for every identifier
/// the placement names ([`unheld_subnet_defect`]), that each of those subnets sits in the VPC
/// the placement names ([`outside_the_vpc_defect`]) and that they span more than one
/// Availability Zone ([`zone_defect`]). They are decided in that order: an identifier that
/// names no subnet of the account says nothing about a VPC or a zone, and a subnet of
/// another VPC is not a placement in this VPC at all whatever zone it happens to be in.
///
/// GROUNDED IN WHAT EC2 ANSWERED, never in its silence — and a subnet the owner of subnets
/// answered NOTHING for IS an answer: the account holds no such subnet, so the placement names
/// a network that does not exist and is refused like any other unservable pick. Only a hop
/// that could not be made at all ([`SubnetResolution::answered`] false — no Region label to ask
/// in, or no owner of subnets mounted to ask) leaves the topology undecided, and then nothing
/// is refused, because this service keeps no network of its own to decide it with.
#[must_use]
pub fn placement_defect(
    resolved: &SubnetResolution,
    subnet_ids: &[String],
    vpc_id: &str,
) -> Option<String> {
    if !resolved.answered {
        return None;
    }
    unheld_subnet_defect(&resolved.networks, subnet_ids)
        .or_else(|| outside_the_vpc_defect(&resolved.networks, subnet_ids, vpc_id))
        .or_else(|| zone_defect(&resolved.networks, subnet_ids))
}

/// Why the subnets a placement names cannot carry a directory's servers — that the account has
/// no subnet for one of the identifiers it named — or `None` when it has one for each.
///
/// `SubnetIds` are "the identifiers of the subnets for the directory servers", so a servable
/// pick is one whose identifiers name subnets of the ACCOUNT: an identifier the owner of
/// subnets holds no subnet for names none, and no directory server can be placed in it.
///
/// The sentence is the invalid-subnets opener ALONE ([`UNHELD_SUBNET_IDS`]): nothing can be
/// said about where a subnet that is not there sits, which is why it states no rule — the same
/// shape as the sibling refusal of a VPC the account does not have
/// ([`super::INVALID_VPC_ID`]), rather than the rule-stating sentences the picks whose subnets
/// the account DOES hold get. A subnet the account HAS is never refused here, whatever the
/// owner left out of its report: that is the business of the halves below.
#[must_use]
fn unheld_subnet_defect(
    resolved: &BTreeMap<String, SubnetNetwork>,
    subnet_ids: &[String],
) -> Option<String> {
    let unheld = subnet_ids
        .iter()
        .any(|subnet_id| !resolved.contains_key(subnet_id));
    unheld.then(|| UNHELD_SUBNET_IDS.to_string())
}

/// Why the subnets a placement names cannot carry the servers of a directory created in
/// `vpc_id` — that EC2 reports one of them in ANOTHER VPC — or `None` when none of them is.
///
/// `DirectoryVpcSettings`' own documentation puts the two members together: `VpcId` is "the
/// identifier of the VPC in which to create the directory" and `SubnetIds` "the identifiers
/// of the subnets for the directory servers", which are the subnets of that VPC. A placement
/// naming a subnet of a different VPC therefore names a network the directory is not being
/// created in, and the servers it asks for could not be placed there.
///
/// Decided over the subnets the account HAS: by the time this is reached every named
/// identifier corresponds to one ([`unheld_subnet_defect`] passed), so what is left to decide
/// is where those subnets sit.
#[must_use]
fn outside_the_vpc_defect(
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

/// Why the zones of the subnets a placement names cannot carry a directory's servers — that
/// EC2 does not report them across more than ONE Availability Zone — or `None` when it does.
///
/// A directory's servers sit in "two subnets \[that\] must be in different Availability
/// Zones", so a servable pick is one the owner of subnets reports a zone for, for every named
/// subnet, in more than one zone: a pick it reports a single zone for cannot hold a server in
/// each of two zones, and one it reports a subnet of WITHOUT a zone cannot be shown to either.
/// Both read the same rule back, so both get its sentence — and the account HOLDS every named
/// subnet by the time this is reached ([`unheld_subnet_defect`] passed), so neither is a
/// statement about whether the subnets are there.
#[must_use]
fn zone_defect(
    resolved: &BTreeMap<String, SubnetNetwork>,
    subnet_ids: &[String],
) -> Option<String> {
    let mut zones: Vec<&str> = Vec::new();
    for subnet_id in subnet_ids {
        let Some(zone) = resolved
            .get(subnet_id)
            .and_then(|subnet| subnet.availability_zone.as_deref())
        else {
            // The owner reported this subnet without the zone the rule turns on, so the pick
            // cannot be shown to spread the servers across two of them. At `warn` because the
            // owner of subnets reports a zone for every subnet it holds: a placement refused
            // here is one that seam stopped answering fully, which is worth seeing.
            ::tracing::warn!(
                %subnet_id,
                "the owner of subnets reported this subnet without an Availability Zone"
            );
            return Some(SUBNETS_IN_ONE_ZONE.to_string());
        };
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
/// all — and different again from [`UNHELD_SUBNET_IDS`], the one a pick naming a subnet the
/// account does not have gets. [`super::refusal`] appends the request id, once.
pub const SUBNETS_IN_ONE_ZONE: &str =
    "Invalid subnet ID(s). The two subnets must be in different Availability Zones.";

/// The diagnostic Directory Service refuses a placement whose subnets do not all sit in the
/// VPC it names with: the invalid-subnets opener this service's recorded placement refusals
/// share, and then the rule the check applies — that both of a placement's subnets belong to
/// the VPC the request named.
///
/// FIXED text, like its siblings — [`SUBNETS_IN_ONE_ZONE`] for a pick in one zone,
/// [`UNHELD_SUBNET_IDS`] for a pick naming a subnet the account does not have and
/// [`super::INVALID_SUBNET_IDS`] for a settings block naming no two distinct subnets — so a
/// caller that breaks any of the placement rules reads one sentence naming its subnet
/// pick as the invalid part of the request and the rule a servable pick satisfies. It quotes
/// neither the subnets nor either VPC. [`super::refusal`] appends the request id, once.
pub const SUBNETS_OUTSIDE_THE_VPC: &str =
    "Invalid subnet ID(s). Both subnets must belong to the specified VPC.";
/// The diagnostic Directory Service refuses a placement naming an identifier the account has no
/// subnet for with.
///
/// FIXED text, and the invalid-subnets opener ALONE: a subnet that is not there has no VPC and
/// no zone to state a rule about, so the refusal of an identifier the account holds no subnet
/// for says only that the identifiers are not the account's. That is the wording this refusal
/// is RECORDED with, and it is the shape [`super::INVALID_VPC_ID`] takes for the sibling pick
/// naming a VPC the account does not have, rather than the rule-stating sentences the picks
/// whose subnets the account DOES hold get. It quotes none of the identifiers it rejected.
/// [`super::refusal`] appends the request id, once.
pub const UNHELD_SUBNET_IDS: &str = "Invalid subnet ID(s).";
