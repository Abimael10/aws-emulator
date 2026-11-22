// SPDX-License-Identifier: CC0-1.0

//! The PUBLIC names a DNS STANDARD RESERVES for documentation, and the reserved addresses they
//! are answered with — the one reading every resolver this emulator serves answers them from.
//!
//! A resolver inside a VPC recurses for a name no zone of the account holds, and this emulator
//! opens no connection to any other host: a lookup here asks nothing and answers out of state
//! this process holds. For a name under somebody's own zone it can therefore establish neither
//! that the name exists nor what it stands for, and inventing an address for it would report a
//! resolution that never happened — the one thing a resolver must not do.
//!
//! A handful of public names are different: a STANDARD, rather than a zone owner, fixes that they
//! exist and reserves them for exactly this use, so a resolver that cannot recurse can still
//! answer them without reporting anything about somebody's zone. Those names are the ones this
//! module answers:
//!
//! * the second-level domains RFC 2606 §3 reserves for documentation ([`DOCUMENTATION_DOMAINS`]),
//!   together with the one host label conventionally published beside each
//!   ([`DOCUMENTATION_HOST_LABEL`]);
//! * answered out of the blocks reserved for documentation addresses: the first address of
//!   `192.0.2.0/24` (RFC 5737 §3, TEST-NET-1) and of `2001:db8::/32` (RFC 3849).
//!
//! The ADDRESSES are this emulator's own stand-in, not the records the registry publishes for
//! those names: a reserved documentation name is answered with a reserved documentation address,
//! which is the one address it is safe to answer with for a name nothing here can look up. They
//! route nowhere, so nothing is reported as reachable that is not: a run that goes on to CONNECT
//! to one is answered by the route, which has none.
//!
//! Every other public name resolves to NOTHING here, which is the other honest answer: the
//! resolver carrying the query reports it the way it reports a name it could not have resolved.
//!
//! This is not a reading of any one name: the rule is the standard's class, every name in it is
//! answered the same way, and no name outside it is special-cased. It lives here, once, so the
//! name service of an execution environment, the DNS server of a VPC and the Resolver data plane
//! of the account cannot disagree about what such a name stands for.

use ::std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The record type an IPv4 address is held under, as a zone's record sets spell one
/// (`ResourceRecordSet$Type`).
pub const RECORD_TYPE_A: &str = "A";

/// The record type an IPv6 address is held under; see [`RECORD_TYPE_A`].
pub const RECORD_TYPE_AAAA: &str = "AAAA";

/// The IPv4 address a documentation name is answered with: the first address of `192.0.2.0/24`,
/// the block RFC 5737 §3 reserves "for use in documentation and example code" (TEST-NET-1).
pub const DOCUMENTATION_ADDRESS: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));

/// The IPv6 address the same names resolve to: the first address of `2001:db8::/32`, the block
/// RFC 3849 reserves for documentation.
pub const DOCUMENTATION_ADDRESS_V6: IpAddr =
    IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 1));

/// The public second-level domains a standard RESERVES for documentation, and so the public names
/// an address can be named for here.
///
/// Source: RFC 2606 §3, "Reserved Example Second Level Domain Names" — `example.com`,
/// `example.net` and `example.org`, "reserved for use in documentation". The TLDs the same
/// document reserves in §2 (`.test`, `.example`, `.invalid`, `.localhost`) are delegated in no
/// root zone, so a name under one of them names no address at all and is not here.
pub const DOCUMENTATION_DOMAINS: &[&str] = &["example.com", "example.net", "example.org"];

/// The one label a documentation domain is conventionally published under BESIDE the domain
/// itself, so a host under it is answered exactly where the domain is and no other label under it
/// names anything.
pub const DOCUMENTATION_HOST_LABEL: &str = "www.";

/// The addresses `name` is answered with by standard, in the order a resolver that prefers IPv4
/// answers them — and EMPTY for every name no standard reserves, which is a name this emulator
/// cannot resolve at all.
#[must_use]
pub fn fixed_addresses(name: &str) -> Vec<IpAddr> {
    if !names_a_fixed_address(name) {
        return Vec::new();
    }
    vec![DOCUMENTATION_ADDRESS, DOCUMENTATION_ADDRESS_V6]
}

/// The addresses `name` resolves to by standard of the record type `record_type` alone: the IPv4
/// address for an `A` question and the IPv6 one for an `AAAA` question.
///
/// Empty for a name no standard fixes an address for, and for a type whose records such a name
/// carries none of — which is a name that EXISTS carrying no record of the type asked about, not
/// a name that is absent ([`names_a_fixed_address`] tells the two apart).
#[must_use]
pub fn fixed_addresses_of_type(name: &str, record_type: &str) -> Vec<IpAddr> {
    let wanted = record_type.trim().to_ascii_uppercase();
    fixed_addresses(name)
        .into_iter()
        .filter(|address| match address {
            IpAddr::V4(_) => wanted == RECORD_TYPE_A,
            IpAddr::V6(_) => wanted == RECORD_TYPE_AAAA,
        })
        .collect()
}

/// Whether a standard reserves `name` for documentation at all — so a question about a type it
/// carries no record of is answered as a name that exists with no such record rather than as one
/// that is not there.
#[must_use]
pub fn names_a_fixed_address(name: &str) -> bool {
    let named = normalized(name);
    let under = named
        .strip_prefix(DOCUMENTATION_HOST_LABEL)
        .unwrap_or(&named);
    DOCUMENTATION_DOMAINS.contains(&under)
}

/// `name` as a name is compared: without the trailing dot that spells the root explicitly, and
/// case-folded — a name is compared without regard to case (RFC 4343).
fn normalized(name: &str) -> String {
    let named = name.trim();
    named
        .strip_suffix('.')
        .unwrap_or(named)
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the names a standard fixes an address for resolve to the documentation addresses,
    /// whichever way they are spelled, and no other public name resolves to anything.
    ///
    /// The rows span what the rule generalises over: each reserved domain, the one host label
    /// published beside it, the spellings of a name (trailing dot, upper case), and the names
    /// outside the class — another label under a reserved domain, a reserved TLD, and an
    /// ordinary public name.
    #[test]
    fn only_the_names_a_standard_fixes_resolve_here() {
        // (case, the name, the addresses it resolves to)
        let rows: &[(&str, &str, &[IpAddr])] = &[
            (
                "the first reserved documentation domain",
                "example.com",
                &[DOCUMENTATION_ADDRESS, DOCUMENTATION_ADDRESS_V6],
            ),
            (
                "the second reserved documentation domain",
                "example.net",
                &[DOCUMENTATION_ADDRESS, DOCUMENTATION_ADDRESS_V6],
            ),
            (
                // The entry the citing issue did not name, asked for by the same rule.
                "the third reserved documentation domain",
                "example.org",
                &[DOCUMENTATION_ADDRESS, DOCUMENTATION_ADDRESS_V6],
            ),
            (
                "the host label published beside one",
                "www.example.org",
                &[DOCUMENTATION_ADDRESS, DOCUMENTATION_ADDRESS_V6],
            ),
            (
                "the same name fully qualified",
                "example.com.",
                &[DOCUMENTATION_ADDRESS, DOCUMENTATION_ADDRESS_V6],
            ),
            (
                "the same name in upper case",
                "EXAMPLE.COM",
                &[DOCUMENTATION_ADDRESS, DOCUMENTATION_ADDRESS_V6],
            ),
            ("another label under one", "broker.example.com", &[]),
            ("a reserved top-level domain", "host.invalid", &[]),
            ("an ordinary public name", "amazon.com", &[]),
            ("the empty name", "", &[]),
        ];
        for (case, name, expected) in rows {
            let resolved = fixed_addresses(name);
            assert_eq!(
                resolved, *expected,
                "case {case:?}: expected {expected:?}, got {resolved:?}"
            );
            assert_eq!(
                names_a_fixed_address(name),
                !expected.is_empty(),
                "case {case:?}: the name exists exactly where an address is fixed for it"
            );
        }
    }

    /// RULE: a question about one record type is answered with the addresses of that type alone,
    /// and a type such a name carries none of is answered with no record.
    #[test]
    fn a_fixed_name_answers_the_type_that_was_asked_for() {
        // (case, the name, the type asked about, the addresses it answers)
        let rows: &[(&str, &str, &str, &[IpAddr])] = &[
            (
                "the IPv4 address of a reserved domain",
                "example.net",
                RECORD_TYPE_A,
                &[DOCUMENTATION_ADDRESS],
            ),
            (
                "the IPv6 address of the same name",
                "example.net",
                RECORD_TYPE_AAAA,
                &[DOCUMENTATION_ADDRESS_V6],
            ),
            (
                "the type spelled in lower case",
                "example.net",
                "aaaa",
                &[DOCUMENTATION_ADDRESS_V6],
            ),
            (
                "a type the name carries no record of",
                "example.net",
                "MX",
                &[],
            ),
            (
                "the IPv4 address of a name no standard fixes",
                "broker.example.net",
                RECORD_TYPE_A,
                &[],
            ),
        ];
        for (case, name, record_type, expected) in rows {
            let answered = fixed_addresses_of_type(name, record_type);
            assert_eq!(
                answered, *expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }
}
