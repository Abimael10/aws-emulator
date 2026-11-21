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
//!   owner: the second-level domains RFC 2606 §3 reserves for documentation, which IANA
//!   publishes address records for ([`DOCUMENTATION_DOMAINS`]), answered out of the blocks
//!   reserved for documentation addresses ([`DOCUMENTATION_ADDRESS`]).
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
//! * a name whose existence and addresses a STANDARD fixes is answered from that standard: the
//!   reserved documentation domains, whose records the registry itself publishes, out of the
//!   address blocks reserved for documentation. The addresses are not routable anywhere, so
//!   nothing is reported as reachable that is not: a run that connects to one is answered by
//!   the route, which has none;
//! * every other public name resolves to nothing, because inventing an address for it would
//!   report a resolution that never happened — the one thing a resolver must not do.
//!
//! What this is NOT is a reading of any one name: the rule is the standard's class, every name
//! in it is answered the same way, and no name outside it is special-cased.

use ::std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use super::vpc_network::VpcAttachment;

/// The address a documentation name resolves to: the first address of `192.0.2.0/24`, the
/// block RFC 5737 §3 reserves "for use in documentation and example code" (TEST-NET-1).
///
/// A name reserved for documentation is answered with an address reserved for the same, so
/// nothing a run resolves here names a host of anything. Nothing of this emulator is served
/// there either, so a run that goes on to CONNECT to it is answered the way a host with no
/// route to an address answers one.
const DOCUMENTATION_ADDRESS: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));

/// The IPv6 address the same names resolve to: the first address of `2001:db8::/32`, the block
/// RFC 3849 reserves for documentation.
const DOCUMENTATION_ADDRESS_V6: IpAddr =
    IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 1));

/// The public second-level domains whose addresses are published by IANA rather than by a zone
/// owner, and so are the public names this resolver can name an address for.
///
/// Source: RFC 2606 §3, "Reserved Example Second Level Domain Names" — `example.com`,
/// `example.net` and `example.org`, "reserved for use in documentation", which the registry
/// itself operates with address records. The TLDs the same document reserves in §2 (`.test`,
/// `.example`, `.invalid`, `.localhost`) are delegated in no root zone, so a name under one of
/// them resolves nowhere: `.localhost` is this machine ([`loopback_name`]) and the other three
/// are answered as the names that do not exist that they are.
const DOCUMENTATION_DOMAINS: &[&str] = &["example.com", "example.net", "example.org"];

/// The one label the registry publishes a documentation domain's addresses under BESIDE the
/// domain itself, so a host under it resolves exactly where it does in the public DNS and no
/// other label under it names anything.
const DOCUMENTATION_HOST_LABEL: &str = "www.";

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
    // An ADDRESS resolves to itself: there is no name to look up.
    if let Ok(address) = named.parse::<IpAddr>() {
        return vec![address];
    }
    if loopback_name(&named) {
        return vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
    }
    let private: Vec<IpAddr> = inside
        .map(|inside| inside.addresses_of(&named))
        .unwrap_or_default()
        .into_iter()
        .map(IpAddr::V4)
        .collect();
    if !private.is_empty() {
        return private;
    }
    if documentation_name(&named) {
        return vec![DOCUMENTATION_ADDRESS, DOCUMENTATION_ADDRESS_V6];
    }
    Vec::new()
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

/// Whether `host` is one of the documentation domains ([`DOCUMENTATION_DOMAINS`]) or the one
/// host label the registry publishes beside it.
fn documentation_name(host: &str) -> bool {
    let under = host.strip_prefix(DOCUMENTATION_HOST_LABEL).unwrap_or(host);
    DOCUMENTATION_DOMAINS.contains(&under)
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
