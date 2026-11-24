// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of the HOST this service ANNOUNCES in a `DnsEntry`, and the entry built
//! on it.
//!
//! `DnsEntry` "describes the DNS information of a service", and its `domainName` is "the
//! domain name of the service", paired with the `hostedZoneId` of "the hosted zone" it
//! lives in (`smithy_query(query="shape",shape="DnsEntry")`); `CreateService`'s `dnsEntry`
//! member is documented "The public DNS name of the service." So the value is a host a
//! client RESOLVES and then DIALS: it feeds it to a resolver, writes it in a `Host` header,
//! uses it as a TLS server name and stores it as the target of a record beside the
//! hosted-zone id it was handed with it.
//!
//! RULE: a host this emulator announces for a client to resolve and dial is the host this
//! emulator IS REACHED AT — the host of the authority the request being answered addressed,
//! or of the one address this process is served at for a caller that addressed a host naming
//! some service ([`announced_dns_host`], read through the tree's one reading of what a client
//! reaches this emulator at, [`::service_common::data_plane_addressing`]) — and never a name
//! derived from it. Nothing resolves a name of this emulator's own invention: a sub-name of
//! the address a client reaches this process at resolves nowhere, and the name AWS hands out
//! lives in a zone AWS serves, which resolves nowhere near this process — or, worse, at
//! genuine AWS infrastructure, which sends every service-to-service call out of the emulated
//! environment carrying the credentials it is holding. The port a client then connects on is
//! the service's LISTENER's, never this member's, so no port travels in the value
//! ([`::service_common::data_plane_addressing::host_only`]).
//!
//! WHICH service a request is for is therefore carried where this API already carries a
//! service's identity — its `id`, its `name` and its `arn`, and, for a client addressing the
//! plane by host, the service's own `customDomainName` or its own LABEL
//! ([`service_label`]), the first label of the hosts this emulator's data plane claims and
//! publishes for a client inside this process (`crate::state::data_plane_addresses`,
//! `crate::service::data_plane`). A client that holds nothing but the announced host reaches
//! the service through the reserved data-plane path on that host
//! (`crate::service::data_plane::data_plane_path`).
//!
//! The entry is built on every read as well as on the create, so nothing persists an address
//! this emulator may no longer be reached at; the hosted-zone id of the entry is the
//! resource's own, minted once and kept on its record.
//!
//! The same reading answers the same question for the sibling member of another service
//! (`service_com_amazonaws_globalaccelerator_globalaccelerator_v20180706::state::announced_dns_host`,
//! for `Accelerator$DnsName`): both compose it out of the shared module above rather than
//! reading a host of their own. What is added here is the NORMALISATION a value this service
//! announces carries — the host lower-cased, with the root dot a `Host` header does not carry
//! left off, and a host of no labels at all answered as the loopback name — so the value is a
//! host however a client wrote the one it addressed.

use ::service_common::data_plane_addressing::{dns_label, host_only, LOOPBACK_ZONE};

/// The first label of a service whose own name and id both fold away to nothing — so the
/// label is a DNS label even for a record no create of this service could store.
const UNNAMED_SERVICE_LABEL: &str = "service";

/// The host every `DnsEntry` of this service announces: the host this emulator is reached at
/// for the request being answered (see the module docs).
///
/// A host and nothing else — no scheme, no path and no `:port`, because a client resolves
/// this value and writes it in a `Host` header; the port it connects on is the service's
/// listener's.
///
/// An emulator served on no port at all has no address of its own, and
/// [`::smithy_server_runtime::OperationContext::endpoint`] names its unserved base, whose
/// host is one nothing resolves — so even then the announced host leads no caller out of
/// this process.
pub fn announced_dns_host(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    let authority = ::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts(
        ctx.parts(),
    )
    .unwrap_or_else(|| ::service_common::data_plane_addressing::emulator_endpoint_authority(ctx));
    host_of_authority(&authority)
}

/// The `DnsEntry` a response carries for a resource whose DNS record holds `hosted_zone_id`:
/// the host this emulator announces ([`announced_dns_host`]) and that stored id.
///
/// Every operation of this service that serializes a `DnsEntry` builds it here, so a create,
/// a read and a listing announce one host.
pub fn announced_dns_entry(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    hosted_zone_id: &str,
) -> crate::ops::types::DnsEntry {
    crate::ops::types::DnsEntry {
        domain_name: Some(announced_dns_host(ctx)),
        hosted_zone_id: Some(hosted_zone_id.to_string()),
    }
}

/// The HOST the `host[:port]` `authority` names: its host as written, without the userinfo
/// and the port a host carries neither of, lower-cased because a host is compared without
/// regard to case, and with the root dot a resolver accepts but a `Host` header does not
/// left off.
///
/// An authority naming no host at all is the [`LOOPBACK_ZONE`] — the name RFC 6761 reserves
/// for the loopback interface, which is the interface the core binds its listeners on unless
/// the operator names another address — so the value is always a host of this process.
fn host_of_authority(authority: &str) -> String {
    let host = host_only(authority)
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if !host.bytes().any(|byte| byte.is_ascii_alphanumeric()) {
        return LOOPBACK_ZONE.to_string();
    }
    host
}

/// One service's own DNS LABEL: its `name` and the distinguishing part of its `id`.
///
/// The label that identifies the SERVICE a host-style data-plane request is for — the first
/// label of every host this emulator's data plane claims and publishes for it
/// (`crate::state::data_plane_addresses`, `crate::service::data_plane`) — so two services of
/// one emulator never share it and a service keeps it for as long as it is stored.
///
/// Both members are already drawn from the characters a label carries (`ServiceName` is
/// `[a-z0-9-]`, `ServiceId` is `svc-[0-9a-z]{17}`), so folding leaves them as they are
/// ([`::service_common::data_plane_addressing::dns_label`], the tree's one reading of a value
/// written as a DNS label); the fold is what keeps the value a label whatever a record holds.
pub fn service_label(name: &str, id: &str) -> String {
    let distinguisher = id.strip_prefix(super::PREFIX_SERVICE).unwrap_or(id);
    let label = dns_label(&format!("{name}-{distinguisher}"));
    if label.is_empty() {
        return UNNAMED_SERVICE_LABEL.to_string();
    }
    label
}

#[cfg(test)]
mod tests {
    use ::service_common::data_plane_addressing::MAX_DNS_LABEL_LENGTH;

    use super::*;

    /// RULE: the host an authority names is its own host, with the userinfo and the port
    /// left behind — never a name derived from it — and the loopback name for an authority
    /// naming no host at all.
    ///
    /// The rows span what the reading generalises over: the authority forms a client reaches
    /// an emulator on (an IPv4 address with and without a port, a bracketed IPv6 literal,
    /// one carrying a zone id, a single-label name, a multi-label name, a name in another
    /// case, a name carrying the root dot, an authority carrying userinfo) and the
    /// degenerate hosts a `Host` header can still carry.
    #[test]
    fn an_authority_announces_its_own_host() {
        let rows: &[(&str, &str, &str)] = &[
            ("an IPv4 address with a port", "127.0.0.1:40263", "127.0.0.1"),
            ("an IPv4 address with no port", "127.0.0.1", "127.0.0.1"),
            ("a routable address", "10.0.0.7:9297", "10.0.0.7"),
            ("a bracketed IPv6 literal", "[::1]:9297", "[::1]"),
            ("an IPv6 literal with a zone id", "[fe80::1%eth0]:9297", "[fe80::1%eth0]"),
            ("the loopback name with a port", "localhost:9297", "localhost"),
            ("a multi-label name", "lattice.emulator.test", "lattice.emulator.test"),
            ("the unserved base's host", "emulator.invalid", "emulator.invalid"),
            ("a name in upper case", "Emulator.INVALID", "emulator.invalid"),
            ("a name carrying the root dot", "emulator.invalid.", "emulator.invalid"),
            ("an authority carrying userinfo", "user@localhost:9297", "localhost"),
            ("a host of empty labels", "..", LOOPBACK_ZONE),
            ("no host at all", "", LOOPBACK_ZONE),
        ];
        for (case, authority, expected) in rows {
            let host = host_of_authority(authority);
            assert_eq!(
                &host, expected,
                "case {case:?}: expected the host {expected:?} for {authority:?}, got {host:?}"
            );
        }
    }

    /// RULE: a service's label is its name and the distinguishing part of its id, as one
    /// DNS label — so it is a label whatever the record holds.
    ///
    /// The rows span what the reading generalises over: the modelled name and id forms
    /// (the shortest and longest `ServiceName`, an id with and without its prefix) and the
    /// values no create of this service stores, which must still fold to a label.
    #[test]
    fn a_service_label_is_one_dns_label() {
        let longest_name = "s".repeat(40);
        let rows: &[(&str, &str, &str, &str)] = &[
            (
                "the shortest modelled name",
                "abc",
                "svc-0123456789abcdef0",
                "abc-0123456789abcdef0",
            ),
            (
                "a hyphenated name",
                "my-lattice-service",
                "svc-0fedcba9876543210",
                "my-lattice-service-0fedcba9876543210",
            ),
            (
                "an id without the modelled prefix",
                "bare",
                "0123456789abcdef0",
                "bare-0123456789abcdef0",
            ),
            ("a name in upper case", "MixedCase", "svc-0a", "mixedcase-0a"),
            ("a name carrying characters no label does", "a_b.c", "svc-0a", "a-b-c-0a"),
            ("no name and no id", "", "", UNNAMED_SERVICE_LABEL),
        ];
        for (case, name, id, expected) in rows {
            let label = service_label(name, id);
            assert_eq!(
                &label, expected,
                "case {case:?}: expected the label {expected:?}, got {label:?}"
            );
        }

        // The longest modelled name plus the longest modelled id suffix still fits in one
        // label, and a longer record is truncated into one.
        let longest = service_label(&longest_name, "svc-0123456789abcdef0");
        assert_eq!(
            longest.len(),
            longest_name.len() + 1 + 17,
            "case \"the longest modelled name\": expected the whole name and id in the \
             label, got {longest:?}"
        );
        let overlong = service_label(&"n".repeat(200), "svc-0123456789abcdef0");
        assert!(
            overlong.len() <= MAX_DNS_LABEL_LENGTH && !overlong.ends_with('-'),
            "case \"a name longer than a label\": expected at most {MAX_DNS_LABEL_LENGTH} \
             characters ending in no hyphen, got {overlong:?}"
        );
    }
}
