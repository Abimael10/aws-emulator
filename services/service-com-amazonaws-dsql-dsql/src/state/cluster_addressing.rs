// SPDX-License-Identifier: CC0-1.0

//! WHERE a cluster's connection endpoints are ANNOUNCED — the value `CreateCluster`
//! publishes and every read of that cluster reports, and the VPC connection endpoint
//! `GetVpcEndpointServiceName` reports beside the endpoint service's name.
//!
//! `Endpoint` is "The connection endpoint for the created cluster"
//! (`smithy_query(query="operation", operation="CreateCluster")`) and `ClusterVpcEndpoint`
//! is "The VPC connection endpoint for the cluster": neither is a label a client reads, but
//! a host it then DIALS to reach that cluster's data plane. So neither may name anything
//! outside this emulator — a name in a zone Amazon Web Services publishes its own endpoints
//! in ([`addressing::AWS_ENDPOINT_DNS_SUFFIXES`]) resolves nowhere near this process or,
//! worse, at genuine infrastructure, so a client handed one leaves the emulated environment
//! carrying the credentials it holds and reaches no cluster at all.
//!
//! What the two members differ in is WHOSE address they are. The cluster's own `endpoint` is
//! the address this process answers its data plane on, so it is the host of the one endpoint
//! it serves its whole surface at ([`addressing::served_endpoint_authority`]) and nothing
//! built around the cluster. A cluster's VPC connection endpoint is the PRIVATE access path
//! to that one cluster, which Amazon Web Services gives a name of its own — the cluster's
//! identifier, the endpoint service in front of it and its Region — so it is composed from
//! the stored cluster ([`cluster_vpc_endpoint`]) under the zone this emulator may announce
//! its own names in ([`announced_zone`]), the loopback zone RFC 6761 reserves for the
//! interface the core binds its listeners on. Each cluster therefore has a VPC host of its
//! own, two reads of one cluster report one host, and the name still leads back here.
//!
//! The member is a HOSTNAME and nothing else — no scheme, no path and no port, which is also
//! all `Endpoint`'s and `ClusterVpcEndpoint`'s `^[a-zA-Z0-9.-]+$` admit
//! ([`is_endpoint_host`]) — so a client dials it on the database's own port, exactly as it
//! does the name Amazon Web Services publishes.
//!
//! What neither carries is the endpoint service's NAME: which endpoint service fronts a
//! cluster is a name and not an address, so it is derived from the cluster's own stored
//! endpoint-service suffix ([`crate::state::vpc_endpoint_service_name`]) rather than from
//! where this process answers.
//!
//! Nothing of a request moves either answer: where this emulator is served is a property of
//! the PROCESS and not of the host a request happened to carry, and the cluster half is read
//! off the stored cluster, so the address the create publishes and the address every later
//! read reports cannot drift from one another.

use ::service_common::data_plane_addressing as addressing;

/// The DNS name a cluster's connection endpoints are announced at, for the emulator serving
/// `ctx`: the host of the one endpoint this process answers on, with no port and no path
/// ([`announced_host`]).
///
/// The reading for the address of the cluster's own DATA PLANE — `CreateCluster`'s and
/// `GetCluster`'s `endpoint`, the host a client opens a database session on — so the address
/// the create publishes cannot drift onto one this process does not answer on. The cluster's
/// VPC connection endpoint is a name of the cluster's own and is composed instead
/// ([`cluster_vpc_endpoint`]).
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

/// Whether `value` is a host `Endpoint`'s `^[a-zA-Z0-9.-]+$` admits
/// (`smithy_query(query="shape", shape="Endpoint")`), which `ClusterVpcEndpoint` — the member
/// `GetVpcEndpointServiceName` reports a cluster's VPC connection endpoint in — declares too.
#[must_use]
pub fn is_endpoint_host(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-'))
}

/// The DNS ZONE this emulator may announce a name of its OWN in, for an emulator whose own
/// endpoint is `served_base`: read off the authority that base names
/// ([`addressing::announced_zone_of_authority`]), which answers the loopback zone for a
/// process reached at an address literal, at no endpoint at all, or at a host Amazon Web
/// Services publishes its own endpoints at.
///
/// RFC 6761 reserves `localhost` and "any names falling within `.localhost.`" for the
/// loopback interface the core binds its listeners on, so a name this service announces
/// under that zone resolves to this process rather than to anything of Amazon Web Services'
/// own.
#[must_use]
pub fn announced_zone(served_base: &str) -> String {
    let served = addressing::served_endpoint_authority(served_base).unwrap_or_default();
    addressing::announced_zone_of_authority(&served)
}

/// The VPC CONNECTION endpoint of one cluster, for the emulator serving `ctx`
/// ([`vpc_endpoint_host`]).
#[must_use]
pub fn cluster_vpc_endpoint(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    identifier: &str,
    endpoint_service_suffix: &str,
    region: &str,
) -> String {
    vpc_endpoint_host(
        ctx.endpoint().as_str(),
        identifier,
        endpoint_service_suffix,
        region,
    )
}

/// The VPC CONNECTION endpoint of one cluster: the cluster's own `identifier` as the leading
/// label, the `dsql-` label carrying that cluster's endpoint-service suffix — the same suffix
/// the reply's `serviceName` names the endpoint service by
/// ([`crate::state::vpc_endpoint_service_name`]) — the cluster's Region, and the zone an
/// emulator served at `served_base` announces its own names in ([`announced_zone`]).
///
/// `ClusterVpcEndpoint` is "The VPC connection endpoint for the cluster"
/// (`smithy_query(query="doc", shape="GetVpcEndpointServiceNameOutput",
/// member="clusterVpcEndpoint")`): a cluster's PRIVATE access path has a name of its own, so
/// the host names the cluster it fronts and the endpoint service in front of it — two reads
/// of one cluster name one host, and two clusters are never reported under the same one. The
/// zone is this emulator's rather than one Amazon Web Services serves, so the name leads back
/// to this process and no client resolving it leaves the emulated environment.
///
/// Every label is folded through [`addressing::dns_label`], so no value a request carried can
/// make the announced host name an authority the caller chose, and the result is a host
/// `ClusterVpcEndpoint`'s `^[a-zA-Z0-9.-]+$` admits ([`is_endpoint_host`]).
#[must_use]
pub fn vpc_endpoint_host(
    served_base: &str,
    identifier: &str,
    endpoint_service_suffix: &str,
    region: &str,
) -> String {
    let identifier = addressing::dns_label(identifier);
    let service = addressing::dns_label(&format!("dsql-{endpoint_service_suffix}"));
    let region = addressing::dns_label(region);
    let zone = announced_zone(served_base);
    format!("{identifier}.{service}.{region}.{zone}")
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

    /// RULE: a cluster's VPC connection endpoint names that cluster — its identifier as the
    /// leading label, then the endpoint service in front of it and the cluster's Region —
    /// under the zone this emulator announces its own names in, whatever base it is served
    /// at.
    ///
    /// The rows span the bases an emulator is served at (a loopback literal, a name, one
    /// with no port, a routable address, no endpoint at all, and a base in a zone Amazon Web
    /// Services serves) and the Regions a cluster is minted in.
    #[test]
    fn a_clusters_vpc_endpoint_names_the_cluster_under_this_emulators_zone() {
        let loopback = addressing::LOOPBACK_ZONE;
        let identifier = "01m7n6yw08s3kcvvjfgrbp7esk";
        let suffix = "7cwu";
        // (case, the base this emulator is served at, the zone it announces in)
        let bases: &[(&str, &str, &str)] = &[
            ("a loopback literal", "https://127.0.0.1:9297", loopback),
            ("a name with a port", "https://emulator.test:39355", "emulator.test"),
            ("a name with no port", "https://emulator-host", "emulator-host"),
            ("a routable address", "https://198.51.100.7:8443", loopback),
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
        for (case, base, zone) in bases {
            for region in ["us-east-1", "eu-west-1", "ap-southeast-2"] {
                let host = vpc_endpoint_host(base, identifier, suffix, region);
                let want = format!("{identifier}.dsql-{suffix}.{region}.{zone}");
                assert_eq!(
                    host, want,
                    "case {case:?} in {region}: expected {want:?}, got {host:?}"
                );
                assert!(
                    is_endpoint_host(&host),
                    "case {case:?} in {region}: {host:?} must satisfy ClusterVpcEndpoint's \
                     pattern"
                );
                assert!(
                    !addressing::addresses_an_aws_endpoint(&host),
                    "case {case:?} in {region}: {host:?} must name no endpoint of Amazon Web \
                     Services' own"
                );
            }
        }
    }
}
