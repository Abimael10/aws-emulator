// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of the DNS NAME this service ANNOUNCES in a `DnsEntry`, and the entry
//! built on it.
//!
//! `DnsEntry` "describes the DNS information of a service", and its `domainName` is "the
//! domain name of the service", paired with the `hostedZoneId` of "the hosted zone" it
//! lives in (`smithy_query(query="shape",shape="DnsEntry")`); `CreateService`'s `dnsEntry`
//! member is documented "The public DNS name of the service." So the member's value space
//! is a DOMAIN NAME: dot-separated labels of letters, digits and hyphens, more than one of
//! them, and nothing else — no scheme, no path, and in particular no `:port`, because a
//! client feeds this value to a resolver, writes it in a `Host` header, uses it as a TLS
//! server name and stores it as the target of a record beside the hosted-zone id it was
//! handed with it. The port a client then connects on is the service's LISTENER's, never
//! this member's.
//!
//! Two halves make the name, so it is a name of the RESOURCE in a zone of THIS process:
//!
//! * the first label names the service, from the service's own `name` and `id`
//!   ([`service_label`]) — the same two values the real service builds a lattice name's
//!   first label out of — so each service has its own name, every read reports the same
//!   one, and no two services share it;
//! * the rest is the ZONE this emulator answers in ([`announced_dns_zone`]), read through
//!   the tree's one reading of what a client reaches this emulator at
//!   ([`::service_common::data_plane_addressing`]): the NAME the request addressed the
//!   emulator on, or the name this process is served at for a caller that addressed a host
//!   naming some service. The name the real service hands out lives in a zone AWS serves,
//!   which resolves nowhere near this process — or, worse, to genuine AWS infrastructure —
//!   so announcing one sends every service-to-service call out of the emulated environment.
//!
//! A request that reached this emulator at an ADDRESS names no zone at all; the name is
//! then minted in [`LOOPBACK_ZONE`], the one zone a resolver answers with the loopback
//! interface — which is the interface the core binds its listeners on unless the operator
//! says otherwise.
//!
//! The entry is built on every read as well as on the create, so nothing persists a zone
//! this emulator may no longer be reached in; the hosted-zone id of the entry is the
//! resource's own, minted once and kept on its record.

use super::{LatticeService, ServiceNetworkServiceAssociation};

/// The zone a service's name is minted in when the request reached this emulator at an
/// ADDRESS, which names no zone: the name reserved for the loopback interface, which is
/// where the core binds its listeners unless the operator names another address.
const LOOPBACK_ZONE: &str = "localhost";

/// The longest a single DNS label may be, in octets.
const MAX_LABEL_LEN: usize = 63;

/// The first label of a service whose own name and id both fold away to nothing — so the
/// announced value is a DNS name even for a record no create of this service could store.
const UNNAMED_SERVICE_LABEL: &str = "service";

/// The service a `DnsEntry` is announced FOR: the two members its DNS name is minted from
/// and the hosted zone its record carries.
///
/// A service announces its own; an association announces the associated SERVICE's, which
/// is why both records hand their identity to one reading
/// ([`LatticeService::announced`], [`ServiceNetworkServiceAssociation::announced_service`])
/// instead of each op assembling the entry itself.
#[derive(Debug, Clone, Copy)]
pub struct AnnouncedService<'a> {
    /// The service's `name`, the first part of its DNS name's first label.
    pub name: &'a str,
    /// The service's `id`, which distinguishes two services a name alone would not.
    pub id: &'a str,
    /// The hosted zone the service's record was minted with, reported as the entry's
    /// `hostedZoneId`.
    pub hosted_zone_id: &'a str,
}

impl LatticeService {
    /// This service's own announced identity: its name, its id and its stored hosted zone.
    pub fn announced(&self) -> AnnouncedService<'_> {
        AnnouncedService {
            name: &self.name,
            id: &self.id,
            hosted_zone_id: &self.dns_hosted_zone_id,
        }
    }
}

impl ServiceNetworkServiceAssociation {
    /// The ASSOCIATED service's announced identity, as the association recorded it: an
    /// association reports the service's own DNS entry, so it announces the service's
    /// name, not one of its own.
    pub fn announced_service(&self) -> AnnouncedService<'_> {
        AnnouncedService {
            name: &self.service_name,
            id: &self.service_id,
            hosted_zone_id: &self.dns_hosted_zone_id,
        }
    }
}

/// The DNS name every `DnsEntry` of this service announces for `service`: the service's
/// own label in the zone this emulator answers in (see the module docs).
///
/// A domain name, always: dot-separated letter-digit-hyphen labels, at least two of them,
/// and never an endpoint authority — no port travels in it.
pub fn announced_dns_name(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    service: AnnouncedService<'_>,
) -> String {
    format!(
        "{}.{}",
        service_label(service.name, service.id),
        announced_dns_zone(ctx)
    )
}

/// The `DnsEntry` a response carries for `service`: the DNS name this emulator announces
/// it under ([`announced_dns_name`]) and the resource's own stored hosted-zone id.
///
/// Every operation of this service that serializes a `DnsEntry` builds it here, so a
/// create, a read and a listing announce one name.
pub fn announced_dns_entry(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    service: AnnouncedService<'_>,
) -> crate::ops::types::DnsEntry {
    crate::ops::types::DnsEntry {
        domain_name: Some(announced_dns_name(ctx, service)),
        hosted_zone_id: Some(service.hosted_zone_id.to_string()),
    }
}

/// The first label of a service's DNS name: its `name` and the distinguishing part of its
/// `id`, which is what makes the announced name the NAME OF THE RESOURCE rather than of
/// the host it is reached at — two services of one emulator never share it, and a service
/// keeps it across every read.
///
/// Both members are already drawn from the characters a label carries (`ServiceName` is
/// `[a-z0-9-]`, `ServiceId` is `svc-[0-9a-z]{17}`), so folding leaves them as they are;
/// the fold is what keeps the announced value a DNS name whatever a record holds.
pub fn service_label(name: &str, id: &str) -> String {
    let distinguisher = id.strip_prefix(super::PREFIX_SERVICE).unwrap_or(id);
    let label = dns_label(&format!("{name}-{distinguisher}"));
    if label.is_empty() {
        return UNNAMED_SERVICE_LABEL.to_string();
    }
    label
}

/// The DNS zone this emulator answers in for the request being served: the NAME it was
/// reached on, or [`LOOPBACK_ZONE`] when it was reached at an address (see the module
/// docs).
///
/// An emulator served on no port at all has no address of its own, and
/// [`::smithy_server_runtime::OperationContext::endpoint`] names its unserved base, whose
/// host is one nothing resolves — so even then the announced name leads no caller out of
/// this process.
fn announced_dns_zone(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    let parts = ctx.parts();
    let host_header = parts
        .headers
        .get(::http::header::HOST)
        .and_then(|value| value.to_str().ok());
    let authority = ::service_common::data_plane_addressing::reached_emulator_endpoint(
        &parts.uri.to_string(),
        host_header,
    )
    .unwrap_or_else(|| ::service_common::data_plane_addressing::emulator_endpoint_authority(ctx));
    zone_of_authority(&authority)
}

/// The zone the `host[:port]` `authority` names: its host, without the port a zone never
/// carries, as DNS labels — or [`LOOPBACK_ZONE`] when the host is an IP ADDRESS, which
/// names no zone, or no labels at all.
fn zone_of_authority(authority: &str) -> String {
    let host = ::service_common::data_plane_addressing::host_only(authority);
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        // An IPv6 host may carry a zone id, which is part of no DNS name.
        .split('%')
        .next()
        .unwrap_or_default();
    if host.parse::<std::net::IpAddr>().is_ok() {
        return LOOPBACK_ZONE.to_string();
    }
    let labels: Vec<String> = host
        .split('.')
        .map(dns_label)
        .filter(|label| !label.is_empty())
        .collect();
    if labels.is_empty() {
        return LOOPBACK_ZONE.to_string();
    }
    labels.join(".")
}

/// `value` as ONE DNS label: lower-cased, every character outside the letters, digits and
/// hyphens a label is written with folded to a hyphen, with no hyphen at either end and no
/// more than the [`MAX_LABEL_LEN`] octets a label may hold.
fn dns_label(value: &str) -> String {
    let folded: String = value
        .chars()
        .map(|character| {
            let character = character.to_ascii_lowercase();
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect();
    folded
        .trim_matches('-')
        .chars()
        .take(MAX_LABEL_LEN)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the zone an authority names is its HOST as DNS labels, with the port left
    /// behind, and the loopback zone for an authority whose host is an address or no
    /// labels at all.
    ///
    /// The rows span what the reading generalises over: the authority forms a client
    /// reaches an emulator on (an IPv4 address with and without a port, a bracketed IPv6
    /// literal, one carrying a zone id, a single-label name, a multi-label name, a
    /// name in another case, an authority carrying userinfo) and the degenerate hosts a
    /// `Host` header can still carry.
    #[test]
    fn an_authority_names_the_zone_of_its_host() {
        let rows: &[(&str, &str, &str)] = &[
            ("an IPv4 address with a port", "127.0.0.1:40263", LOOPBACK_ZONE),
            ("an IPv4 address with no port", "127.0.0.1", LOOPBACK_ZONE),
            ("a routable address", "10.0.0.7:9297", LOOPBACK_ZONE),
            ("a bracketed IPv6 literal", "[::1]:9297", LOOPBACK_ZONE),
            ("an IPv6 literal with a zone id", "[fe80::1%eth0]:9297", LOOPBACK_ZONE),
            ("the loopback name with a port", "localhost:9297", "localhost"),
            ("a multi-label name", "lattice.emulator.test", "lattice.emulator.test"),
            ("the unserved base's host", "emulator.invalid", "emulator.invalid"),
            ("a name in upper case", "Emulator.INVALID", "emulator.invalid"),
            ("an authority carrying userinfo", "user@localhost:9297", "localhost"),
            ("a host of empty labels", "..", LOOPBACK_ZONE),
            ("no host at all", "", LOOPBACK_ZONE),
        ];
        for (case, authority, expected) in rows {
            let zone = zone_of_authority(authority);
            assert_eq!(
                &zone, expected,
                "case {case:?}: expected the zone {expected:?} for {authority:?}, got {zone:?}"
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
            overlong.len() <= MAX_LABEL_LEN && !overlong.ends_with('-'),
            "case \"a name longer than a label\": expected at most {MAX_LABEL_LEN} \
             characters ending in no hyphen, got {overlong:?}"
        );
    }
}
