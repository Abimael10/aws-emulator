// SPDX-License-Identifier: CC0-1.0

//! WHERE a cluster's connection endpoint is ANNOUNCED — the one value `CreateCluster`
//! persists on a cluster and every read of that cluster reports.
//!
//! `Endpoint` is "The connection endpoint for the created cluster"
//! (`smithy_query(query="operation", operation="CreateCluster")`): not a label a client
//! reads, but the host it then DIALS to reach that cluster's data plane. So the only thing
//! the member may carry is an address THIS emulator answers on — the host of the one
//! endpoint it serves its whole surface at ([`addressing::served_endpoint_authority`]).
//!
//! A name in a zone Amazon Web Services publishes its own endpoints in
//! ([`addressing::AWS_ENDPOINT_DNS_SUFFIXES`]) resolves nowhere near this process or, worse,
//! at genuine infrastructure, so a client handed one leaves the emulated environment
//! carrying the credentials it holds and reaches no cluster at all. A name of this
//! emulator's OWN invention is no better: nothing answers for a sub-name of the host this
//! process is reached at, so a client that resolves one reaches nothing either.
//!
//! ONE host therefore answers for every cluster here, where Amazon Web Services gives each
//! cluster a dedicated name: this process serves its whole surface at a single endpoint, and
//! which cluster a session is on is what the connection itself carries. The member is a
//! HOSTNAME and nothing else — no scheme, no path and no port, which is also all
//! `Endpoint`'s `^[a-zA-Z0-9.-]+$` admits ([`is_endpoint_host`]) — so a client dials it on
//! the database's own port, exactly as it does the name Amazon Web Services publishes.
//!
//! Nothing of a request moves the answer: where this emulator is served is a property of the
//! PROCESS and not of the host a request happened to carry, so the address the create
//! publishes and the address every later read reports cannot drift from one another.

use ::service_common::data_plane_addressing as addressing;

/// The DNS name a cluster's connection endpoint is announced at, for the emulator serving
/// `ctx`: the host of the one endpoint this process answers on, with no port and no path
/// ([`announced_host`]).
///
/// What it is read from is the tree's one statement of where this emulator answers
/// ([`::smithy_server_runtime::OperationContext::endpoint`], "the base URL a later request
/// addresses… Callers send normally signed requests to it"), so this service states no
/// second rule for it.
#[must_use]
pub fn cluster_endpoint(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    announced_host(ctx.endpoint().as_str())
}

/// The host a cluster is announced at for an emulator whose own endpoint is `served_base`:
/// the authority that base names ([`addressing::served_endpoint_authority`]) without its
/// port ([`addressing::host_only`]), because the member carries a name a client dials on the
/// database's own port rather than an address carrying one.
///
/// An emulator that is served on no endpoint at all (one embedded in another process, whose
/// base names a host RFC 6761 reserves against resolution), and one bound where the host is
/// written in a form a hostname member cannot carry (a bracketed IPv6 literal), announce the
/// loopback zone instead ([`addressing::LOOPBACK_ZONE`]) — the interface the core binds its
/// listeners on, and the only name left that leads back here.
#[must_use]
pub fn announced_host(served_base: &str) -> String {
    let served = addressing::served_endpoint_authority(served_base).unwrap_or_default();
    let host = addressing::host_only(&served);
    if is_endpoint_host(host) && !addressing::addresses_an_aws_endpoint(host) {
        return host.to_string();
    }
    addressing::LOOPBACK_ZONE.to_string()
}

/// The DNS zone this emulator may ANNOUNCE a name of its OWN in, for the emulator
/// serving `ctx`: the zone of the address it is reached at, and the loopback zone
/// — the interface the core binds its listeners on — where that address names no
/// zone of its own.
///
/// What a cluster announces a per-cluster NAME in, where the member carries a host
/// built around the cluster rather than the one address this process answers at
/// ([`crate::state::cluster_vpc_endpoint`]). The reading is the tree's one
/// ([`addressing::announced_zone_of_authority`]): a name in a zone Amazon Web
/// Services publishes its own endpoints in is never announced, because a client
/// that resolves one leaves the emulated environment carrying the credentials it
/// holds, while a name under the loopback zone — which RFC 6761 reserves, with
/// every name under it, for the loopback interface — leads back to the interface
/// the core opened the cluster's listener on.
#[must_use]
pub fn announced_zone(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    let served = addressing::served_endpoint_authority(ctx.endpoint().as_str()).unwrap_or_default();
    addressing::announced_zone_of_authority(&served)
}

/// Whether `value` is a host `Endpoint`'s `^[a-zA-Z0-9.-]+$` admits
/// (`smithy_query(query="shape", shape="Endpoint")`), which `ClusterVpcEndpoint` — the member
/// `GetVpcEndpointServiceName` reports the same stored host in — declares too.
#[must_use]
pub fn is_endpoint_host(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the announced host is the host this emulator is served at, whatever form its
    /// endpoint takes — never a port, a scheme, a host no hostname member can carry, or a
    /// host Amazon Web Services publishes its own endpoints at.
    ///
    /// The rows span the forms an endpoint base takes: a loopback literal with a port, a
    /// name with another port, a name with no port, a routable address, a bracketed IPv6
    /// literal (which no hostname member can carry), the base of an emulator served on no
    /// port at all, and a base in a zone Amazon Web Services serves.
    #[test]
    fn the_announced_host_is_the_host_this_emulator_is_served_at() {
        let loopback = addressing::LOOPBACK_ZONE;
        // (case, the base this emulator is served at, the host it announces)
        let rows: &[(&str, &str, &str)] = &[
            ("a loopback literal", "https://127.0.0.1:9297", "127.0.0.1"),
            ("a name with a port", "https://localhost:39355", "localhost"),
            ("a name with no port", "https://emulator-host", "emulator-host"),
            ("a routable address", "https://198.51.100.7:8443", "198.51.100.7"),
            ("a bracketed IPv6 literal", "https://[::1]:8443", loopback),
            (
                "an emulator served on no port",
                ::smithy_server_runtime::UNSERVED_URL_BASE,
                loopback,
            ),
            (
                "a base in a zone Amazon Web Services serves",
                "https://dsql.us-west-2.on.aws",
                loopback,
            ),
        ];
        for (case, base, want) in rows {
            let announced = announced_host(base);
            assert_eq!(
                announced, *want,
                "case {case:?}: expected {want:?}, got {announced:?}"
            );
            assert!(
                is_endpoint_host(&announced),
                "case {case:?}: {announced:?} must satisfy Endpoint's pattern"
            );
        }
    }

    /// RULE: `Endpoint`'s pattern admits a hostname and nothing else — not a port, not a
    /// scheme, not a path, and not the brackets an IPv6 literal is written in.
    #[test]
    fn the_endpoint_pattern_admits_a_hostname_and_nothing_else() {
        // (case, the value, whether the pattern admits it)
        let rows: &[(&str, &str, bool)] = &[
            ("a loopback literal", "127.0.0.1", true),
            ("the loopback zone", "localhost", true),
            ("a hyphenated name", "emulator-host.internal", true),
            ("a host carrying a port", "127.0.0.1:9297", false),
            ("a bracketed IPv6 literal", "[::1]", false),
            ("a URL", "https://127.0.0.1:9297", false),
            ("a host carrying userinfo", "user@127.0.0.1", false),
            ("nothing at all", "", false),
        ];
        for (case, value, want) in rows {
            let admitted = is_endpoint_host(value);
            assert_eq!(
                admitted, *want,
                "case {case:?}: expected {want:?} for {value:?}, got {admitted:?}"
            );
        }
    }
}
