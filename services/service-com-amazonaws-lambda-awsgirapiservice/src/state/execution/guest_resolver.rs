// SPDX-License-Identifier: CC0-1.0

//! The NAME SERVICE of a running handler: which addresses a host name resolves to inside the
//! execution environment.
//!
//! A handler resolves names — `socket.getaddrinfo`, `socket.gethostbyname`, and every client
//! library built on them — and what answers it is the resolver of the environment it runs in.
//! This module is that resolver: ONE reading of a name, so the forward lookups a handler makes
//! and the route a socket of its own is given ([`super::guest_socket`]) cannot disagree about
//! what a name stands for.
//!
//! It answers out of the zones this emulator can name an address in, and out of nothing else:
//!
//! * an ADDRESS is a conversion rather than a lookup — the address itself is the answer;
//! * this machine's own loopback names, which is where the planes this emulator opens are
//!   served, and every name under the `localhost` domain RFC 6761 §6.3 reserves for it;
//! * the private zone of the VPC the function is ATTACHED to, asked of that VPC's own DNS
//!   server as a client asks it ([`super::vpc_network::VpcAttachment::addresses_of`]) — so the
//!   Amazon-provided hostname of an address in the VPC resolves to that address;
//! * the public names whose existence the DNS standards themselves FIX rather than a zone
//!   owner, answered out of the one reading of them this tree holds
//!   (`service_common::public_dns_standard`) — so the name service of this environment, the DNS
//!   server of the VPC it is attached to and the Resolver data plane of the account cannot
//!   disagree about what such a name stands for.
//!
//! A name no zone it holds names an address under is answered the way a resolver answers a
//! name that does not exist: nothing, which the caller reports as the name-resolution error
//! (`EAI_NONAME`) a program branches on. Resolving a name is not reaching it — what a
//! connection to the resolved address comes to is the route's own answer, and an address
//! nothing of this emulator is served at has no route from here.
//!
//! # Why the public namespace stops at the names a standard fixes
//!
//! A function's sandbox on AWS has a recursing resolver, so it resolves whatever the public DNS
//! holds. This emulator opens no connection to any other host — a lookup here asks nothing and
//! answers out of state this process holds — so for a name under somebody's own zone it cannot
//! establish either that the name exists or what it stands for. The two honest answers to such
//! a name are therefore the ones this module gives:
//!
//! * a name whose existence and addresses a STANDARD fixes is answered from that standard
//!   (`service_common::public_dns_standard`, which states which names those are and why). The
//!   addresses are not routable anywhere, so nothing is reported as reachable that is not: a run
//!   that connects to one is answered by the route, which has none;
//! * every other public name resolves to nothing, because inventing an address for it would
//!   report a resolution that never happened — the one thing a resolver must not do.
//!
//! What this is NOT is a reading of any one name: the rule is the standard's class, every name
//! in it is answered the same way, and no name outside it is special-cased.

use ::std::net::{IpAddr, Ipv4Addr};

use super::vpc_network::VpcAttachment;

/// The addresses `host` resolves to inside the execution environment, in the order a resolver
/// answers them: the IPv4 address of a name first, as a resolver that prefers it does.
///
/// Empty for a name no zone this resolver holds names an address under, which is the answer a
/// caller reports as a name that does not exist. `inside` is the VPC the function is attached
/// to, and `None` for a function attached to none — which then has no private zone to resolve
/// a name in.
#[must_use]
pub fn addresses_of(inside: Option<&VpcAttachment>, host: &str) -> Vec<IpAddr> {
    let named = normalized(host);
    if named.is_empty() {
        return Vec::new();
    }
    // A value that spelled the root TWICE still carries a trailing dot after the one form a
    // resolver is handed has been taken off it ([`normalized`]): it is malformed, and it stays
    // malformed here rather than being read as the well-formed name it resembles.
    if named.ends_with('.') {
        return Vec::new();
    }
    // An ADDRESS resolves to itself: there is no name to look up.
    if let Ok(address) = named.parse::<IpAddr>() {
        return vec![address];
    }
    if loopback_name(&named) {
        return vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
    }
    // What the VPC's OWN resolver answers for the name, asked of it as a client asks: the
    // private hosted zones associated with the VPC, its internal zone, and the public names a
    // standard fixes, all in one reading.
    let private: Vec<IpAddr> = inside
        .map(|inside| inside.addresses_of(&named))
        .unwrap_or_default();
    if !private.is_empty() {
        return private;
    }
    // The names a DNS standard fixes an address for, out of the one reading of them this tree
    // holds: a function attached to a VPC is answered them by that VPC's own resolver above,
    // which reads the same table, and one attached to no VPC — which has no resolver to ask — is
    // answered them here.
    ::service_common::public_dns_standard::fixed_addresses(&named)
}

/// Whether `host` names THIS machine: one of the loopback names the core answers for, or a
/// name under the `localhost` domain — "the domain name 'localhost.' and any names falling
/// within '.localhost.' … resolve to loopback addresses" (RFC 6761 §6.3).
///
/// The tree's one reading of that question
/// (`service_common::data_plane_addressing::names_the_loopback_interface`), so a name this
/// resolver answers with the loopback address is the same set of names the register of the
/// planes this emulator serves resolves there (`service_common::dialable_data_planes`).
fn loopback_name(host: &str) -> bool {
    ::service_common::data_plane_addressing::names_the_loopback_interface(host)
}

/// `host` as a name is looked up: without the brackets an address literal may be written in,
/// without the trailing dot that spells the root explicitly, and case-folded — a name is
/// compared without regard to case (RFC 4343).
///
/// Exactly ONE bracket pair and one trailing dot are taken off, because that is the one form a
/// resolver is handed: a value spelling either twice is malformed, and it stays malformed here
/// rather than being read as the well-formed name it resembles.
fn normalized(host: &str) -> String {
    let named = host.trim();
    let named = named
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(named);
    named
        .strip_suffix('.')
        .unwrap_or(named)
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests;
