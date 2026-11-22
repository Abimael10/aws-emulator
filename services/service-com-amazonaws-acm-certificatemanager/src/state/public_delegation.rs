// SPDX-License-Identifier: CC0-1.0

//! Which hosted zone of the account the PUBLIC DNS answers a name from.
//!
//! A certificate's DNS challenge is resolved by ACM from the public DNS hierarchy down,
//! not by looking inside the account: `DomainValidation$ResourceRecord` "contains the
//! CNAME record that you add to your DNS database for domain validation", and it proves
//! control of the name only where a resolver starting at the root reaches the zone the
//! record was written in. A hosted zone is a set of name servers nobody is sent to until
//! something above it delegates the name — so a zone nothing delegates to is answered by
//! nobody, and a record published in it resolves nowhere and proves nothing.
//!
//! So this module resolves the name the way a resolver does, over the delegations this
//! emulator holds:
//!
//! * the REGISTRY's delegation of the name's registrable part, held by the registrar
//!   service: registering a domain "[c]reates a Route 53 hosted zone that has the same
//!   name as the domain. Route 53 assigns four name servers to your hosted zone and
//!   automatically updates your domain registration with the names of these name
//!   servers" (`RegisterDomain` doc), so a registration of the account's is what puts a
//!   name under a delegated top-level domain into the public hierarchy, pointed at this
//!   account's DNS. A name nobody registered — or one another registrant holds — is
//!   delegated to nobody here, whatever zones the account creates for it;
//! * each zone's own delegations, held by the DNS service: an `NS` record set BELOW a
//!   zone's apex is a zone cut, and resolution follows it to whichever zone of the
//!   account serves those name servers. A deeper zone no parent cuts to is never
//!   reached, exactly as in the public DNS.
//!
//! A name the registries RESERVE rather than delegate ([`domain_is_not_in_public_dns`])
//! has no registration to read, because no registrar sells it: the account's own public
//! zone is then the only authority there is for it — the same rule
//! [`first_unvalidatable_name_in`] decides such a name's holder by.
//!
//! Nothing here is copied into a certificate's record: the delegation is READ on every
//! pass, so a registration or an `NS` record set that changes moves what the next pass
//! resolves with it.

use super::*;

use ::service_com_amazonaws_route53_awsdnsv20130401::state as dns_store;
use ::service_com_amazonaws_route53domains_route53domains_v20140515::state as registrar_store;

/// The DNS service's store, which holds the account's hosted zones.
pub(super) type Zones = dns_store::ComAmazonawsRoute53Awsdnsv20130401State;

/// The registrar service's store, which holds the account's registrations.
pub(super) type Registrations =
    registrar_store::ComAmazonawsRoute53domainsRoute53domainsV20140515State;

/// The record type a zone cut is published as: the `NS` member of the DNS service's
/// `RRType` enum, whose values are the types a record set may carry.
const RECORD_TYPE_NS: &str = "NS";

/// The PUBLIC hosted zone of the account that answers for `name` in the public DNS, or
/// `None` when no resolver starting at the root reaches a zone of the account at all.
///
/// Resolution starts where the authority above the account delegates the name
/// ([`root_authority_for`]) and then follows every zone cut below it, exactly as a
/// resolver walks referrals down to the authoritative zone. The zone it ends at is the
/// one whose records a reader of `name` sees; a record in any other zone of the account
/// sits where the public DNS never asks.
pub(super) fn authoritative_zone_for<'zones>(
    name: &str,
    zones: &'zones Zones,
    registrations: Option<&Registrations>,
) -> Option<&'zones dns_store::StoredZone> {
    let mut authority = root_authority_for(name, zones, registrations)?;
    // One step per label of the name is more cuts than the name can have, so the walk
    // terminates whatever the stores hold.
    for _ in 0..labels_of(name).len() {
        let Some(servers) = delegation_below(authority, name) else {
            return Some(authority);
        };
        let child = zone_served_by(&servers, name, zones)?;
        if child.id == authority.id {
            return Some(authority);
        }
        authority = child;
    }
    Some(authority)
}

/// The zone resolution of `name` STARTS at: the account's outermost public zone covering
/// the name, once something above the account delegates the name to it at all.
///
/// Under a top-level domain the registries DELEGATE, that something is the registry, and
/// the account holds its delegation only through a registration of the name's registrable
/// part — which Route 53 points at this account's own DNS (`RegisterDomain` doc). A name
/// no registration of the account covers is delegated elsewhere, so no zone the account
/// creates for it is ever asked.
///
/// A name the registries RESERVE ([`domain_is_not_in_public_dns`]) has no registry
/// delegation to hold: nobody registers such a name, so the account's own public zone is
/// the only authority for it, which is the same rule
/// [`first_unvalidatable_name_in`] reads to decide that such a name has a holder at all.
///
/// The OUTERMOST covering zone is where the delegation lands — the zone that answers for
/// the registrable name on down — and each cut below it is then followed in turn, so a
/// deeper zone is reached only through the referral its parent publishes.
fn root_authority_for<'zones>(
    name: &str,
    zones: &'zones Zones,
    registrations: Option<&Registrations>,
) -> Option<&'zones dns_store::StoredZone> {
    // The reservation rule is stated over a domain name, so the name is asked without
    // the root dot a fully qualified record name carries.
    if !domain_is_not_in_public_dns(&labels_of(name).join(".")) {
        let registrable = registrable_name(name)?;
        registrations?.domains.get(&registrable)?;
    }
    covering_zones(name, zones).min_by_key(|zone| labels_of(&zone.name).len())
}

/// The zone cut on the way from `authority`'s apex down to `name`, as the name servers it
/// delegates to, or `None` when `authority` answers for `name` itself.
///
/// A zone's own records end at its cuts, so the SHALLOWEST `NS` record set below the apex
/// is the referral a resolver is given; the apex's own `NS` set is the zone's delegation
/// from its parent rather than a cut inside it, so the walk starts one label below the
/// apex.
fn delegation_below(authority: &dns_store::StoredZone, name: &str) -> Option<Vec<String>> {
    let labels = labels_of(name);
    let apex_depth = labels_of(&authority.name).len();
    for depth in (apex_depth + 1)..=labels.len() {
        let candidate = labels[labels.len() - depth..].join(".");
        let delegated = authority.records_at(&candidate, RECORD_TYPE_NS);
        if delegated.is_empty() {
            continue;
        }
        return Some(
            delegated
                .iter()
                .flat_map(|record| record.resource_records.iter().flatten())
                .map(|data| dns_store::canonical_dns_name(&data.value))
                .collect(),
        );
    }
    None
}

/// The PUBLIC zone of the account that `servers` delegate `name` to: the zone covering
/// the name that one of those name servers serves.
///
/// A cut names name servers rather than a zone, so what answers is whichever zone of the
/// account serves them — read through the DNS service's own view of a zone's delegation
/// set, so a zone created in a reusable one is matched by the set it was created in. Name
/// servers no zone of the account serves lead outside this emulator, where nothing of the
/// account's is published.
fn zone_served_by<'zones>(
    servers: &[String],
    name: &str,
    zones: &'zones Zones,
) -> Option<&'zones dns_store::StoredZone> {
    covering_zones(name, zones)
        .filter(|zone| {
            zones
                .zone_delegation_set(zone)
                .name_servers
                .iter()
                .any(|served| servers.contains(&dns_store::canonical_dns_name(served)))
        })
        .max_by_key(|zone| labels_of(&zone.name).len())
}

/// Every PUBLIC hosted zone of the account whose apex covers `name`.
///
/// A private zone is "visible only in the VPCs that you specify"
/// (`HostedZoneConfig$PrivateZone`), so it answers nowhere the public DNS can see it and
/// is no candidate at all. Which names one zone covers is the DNS service's own rule
/// (`name_is_in_zone`), which compares label by label, so the trailing dot and the letter
/// case a zone was created with do not decide it and a sibling name is not inside the
/// zone.
fn covering_zones<'zones>(
    name: &str,
    zones: &'zones Zones,
) -> impl Iterator<Item = &'zones dns_store::StoredZone> {
    let name = name.to_string();
    zones
        .zones
        .values()
        .filter(move |zone| !zone.private_zone && dns_store::name_is_in_zone(&name, &zone.name))
}

/// The REGISTRABLE part of `name`: the one label under the public suffix the registries
/// delegate names beneath, which is the name a registration can be held for.
///
/// Asked of the registrar service's own resolution of a `DomainName`
/// (`resolve_domain_name`), which answers exactly for a name a registry accepts a
/// registration of — so the public-suffix rule that decides where registrations sit stays
/// single-homed in the crate that owns it. The LONGEST suffix of the name that resolves is
/// the registrable one, so a multi-label public suffix is handled by the same walk as a
/// single-label one, and a label no registration could carry (an underscored challenge
/// label, a wildcard) is skipped rather than mistaken for one.
fn registrable_name(name: &str) -> Option<String> {
    let labels = labels_of(name);
    (2..=labels.len())
        .rev()
        .map(|depth| labels[labels.len() - depth..].join("."))
        .find_map(|candidate| registrar_store::resolve_domain_name(&candidate).ok())
}

/// The labels of `name`, in the canonical form the DNS service compares names in
/// (`canonical_dns_name`), so the trailing dot, the letter case and the escaping a name
/// was written with do not change the labels it has.
fn labels_of(name: &str) -> Vec<String> {
    dns_store::canonical_dns_name(name)
        .split('.')
        .filter(|label| !label.is_empty())
        .map(str::to_string)
        .collect()
}
