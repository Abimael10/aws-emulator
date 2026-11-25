// SPDX-License-Identifier: CC0-1.0

//! The ADDRESSES a domain is announced at — `Domain.domainEndpointUrl` and every
//! `Domain.customEndpointUrls` entry — the route that answers there, and the page a fetch
//! of one of them renders.
//!
//! `domainEndpointUrl` is "The HTTPS endpoint URL for accessing the domain" and
//! `customEndpointUrls` is "Additional endpoint URLs derived from the domain name"
//! (`smithy_query(query="shape",shape="Domain")`). Neither is a label a caller reads back
//! and puts away: both are URLs a browser, a probe or a deep-link flow OPENS, and a domain
//! is addressed by HOST. The LEFTMOST LABEL of that host is the resource's own identity —
//! the domain's id for the endpoint "for accessing the domain", the domain's name for each
//! endpoint "derived from the domain name" — and the labels after it are one suffix both
//! members share. A client recovers which domain an endpoint belongs to by reading that
//! first label, matches a virtual-host or TLS name on it, and derives a name endpoint from a
//! name, so the identity lives in the host ([`announced_endpoint_authority`]) and never in a
//! path.
//!
//! The SUFFIX is the only part of the host this emulator chooses, and it is the zone this
//! emulator may announce a name of its own in, read off the `host[:port]` the request was
//! reached at ([`announced_authority`],
//! `::service_common::data_plane_addressing::announced_zone_of_authority`): that address's
//! own host, or the loopback zone RFC 6761 reserves — which a resolver answers with the
//! loopback interface, names under it included — when the emulator is reached at an IP
//! address. The announced port is the one the request arrived on, so the address leads back
//! to THIS process at the socket it is answered on rather than to a zone AWS serves, where a
//! caller would leave the emulated environment altogether.
//!
//! A client configured with ONE endpoint resolves no name at all, so the same domain is ALSO
//! served under a path of this emulator's own endpoint ([`domain_endpoint_path`],
//! [`domain_name_endpoint_path`]) — behind the reserved namespace segment every
//! emulator-served plane sits under
//! (`::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE`) and the segment that
//! names this service's own plane. That path is a second address of the same page, not what a
//! response announces.
//!
//! The members are composed at PROJECTION time, per request ([`announced_domain`]), and the
//! composition is never written by a read: a domain read through one address is never
//! answered another address's, and two deployments of this emulator answer on two
//! addresses, so they announce two sets.
//!
//! One function mints each URL and one reads an address back
//! ([`addressed_domain_endpoint`]), and `crate::service` SERVES exactly those addresses from
//! the stored domain, so what a response advertises and what this emulator answers cannot
//! drift apart.
//!
//! Nothing here is ever CONNECTED to: the module formats strings into a response and holds
//! no outbound client, socket or resolver.

use crate::ops::types::Domain;

/// The segment that names THIS service's domain plane, behind the reserved namespace
/// segment: `/_aws/cloudwatch-omni/…`.
///
/// This service's own endpoint prefix, so a path under that namespace naming another
/// service's plane is never read as one of this service's addresses.
pub const DOMAIN_PLANE: &str = super::ENDPOINT_PREFIX;

/// The segment that says a path addresses a domain BY ID — the identity the announced
/// `domainEndpointUrl` host carries as its first label.
pub const DOMAIN_BY_ID_SEGMENT: &str = "domain";

/// The segment that says a path addresses a domain BY NAME — the identity the host of each
/// announced `customEndpointUrls` entry carries, the member being "derived from the domain
/// name".
pub const DOMAIN_BY_NAME_SEGMENT: &str = "domain-name";

/// The request path the domain `domain_id` is ALSO served at, on this emulator's own
/// endpoint, for a client that resolves no name but the one endpoint it is configured with.
///
/// The second address of the page an announced URL leads to, never what a response
/// announces: the identity of an announced endpoint is its host's first label
/// ([`announced_endpoint_authority`]).
///
/// A `DomainId` is admitted only by `^d-[0-9a-z]{1,25}$` (the model enforces it at the
/// request boundary and this service mints ids of that shape), so every character it can
/// carry is already a path character and the id needs no escaping.
#[must_use]
pub fn domain_endpoint_path(domain_id: &str) -> String {
    format!(
        "/{}/{DOMAIN_PLANE}/{DOMAIN_BY_ID_SEGMENT}/{domain_id}",
        ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE
    )
}

/// The request path the domain NAMED `name` is ALSO served at, on this emulator's own
/// endpoint — the name-addressed counterpart of [`domain_endpoint_path`], and no more what a
/// response announces than that one is.
///
/// `Domain$name` is admitted only by `^[a-z0-9]+(-[a-z0-9]+)*$` (3-63 characters, enforced
/// at the request boundary), so the name is a DNS label's worth of characters and needs no
/// escaping either.
#[must_use]
pub fn domain_name_endpoint_path(name: &str) -> String {
    format!(
        "/{}/{DOMAIN_PLANE}/{DOMAIN_BY_NAME_SEGMENT}/{name}",
        ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE
    )
}

/// The `host[:port]` every endpoint URL this service announces is rooted on, for the
/// request `ctx` is serving.
///
/// Read in the order every announced address of this workspace reads it:
///
/// * a request that addressed the EMULATOR'S OWN endpoint
///   (`::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts`) comes
///   from a client that resolves nothing else, so that is the address it reached — port
///   included, because the published value is what a client dials;
/// * a request on an emulator that IS served on a port
///   (`::service_common::data_plane_addressing::served_endpoint_authority`) — a caller that
///   addressed one of AWS's own hosts, an in-process call of another service included — is
///   answered the one address this process answers on;
/// * an emulator served on no port at all has one base and that base is read, so the value
///   still names this emulator rather than a host belonging to someone else.
#[must_use]
pub fn announced_authority(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    let base = ctx.endpoint();
    ::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts(ctx.parts())
        .or_else(|| {
            ::service_common::data_plane_addressing::served_endpoint_authority(base.as_str())
        })
        .unwrap_or_else(|| {
            ::service_common::data_plane_addressing::url_authority(base.as_str())
                .trim_end_matches('/')
                .to_string()
        })
}

/// The scheme every endpoint URL a domain publishes carries: both members are documented
/// HTTPS endpoints.
pub const DOMAIN_ENDPOINT_SCHEME: &str = "https";

/// The `host[:port]` a domain is ANNOUNCED at, for an emulator reached at
/// `reached_authority` and an `identity` that names the domain: `identity` as the LEFTMOST
/// LABEL, then the one suffix every endpoint of this emulator shares, then the port the
/// request arrived on.
///
/// The suffix is the zone this emulator may announce a name of its own in
/// (`::service_common::data_plane_addressing::announced_zone_of_authority`, the workspace's
/// one reading of that question), read off the address this request reached it at
/// ([`announced_authority`]): the loopback zone where that address is an IP literal rather
/// than a name, which a resolver answers with the interface the core bound. So the two
/// members of one domain, and the members of every domain, share one suffix, and the label in
/// front of it is the only thing that says WHICH domain — which is the relation a client
/// reads an endpoint back through.
///
/// The PORT is the one the request was answered on, carried through so a client dials the
/// socket this emulator answers at rather than the implicit HTTPS port nothing serves.
///
/// The identity is taken only as ONE DNS LABEL
/// (`::service_common::data_plane_addressing::dns_label`, the tree's one statement of that
/// grammar). A `DomainId` (`^d-[0-9a-z]{1,25}$`) and a `Domain$name`
/// (`^[a-z0-9]+(-[a-z0-9]+)*$`, 3-63 characters) are labels already, so folding changes
/// neither; it is what keeps any other stored value from naming an authority of its own in an
/// announced URL. A value that folds to no label at all names no endpoint, so the address
/// stays this emulator's own.
#[must_use]
pub fn announced_endpoint_authority(reached_authority: &str, identity: &str) -> String {
    let label = ::service_common::data_plane_addressing::dns_label(identity);
    if label.is_empty() {
        return reached_authority.to_string();
    }
    let host = format!(
        "{label}.{}",
        ::service_common::data_plane_addressing::announced_zone_of_authority(reached_authority)
    );
    match ::service_common::data_plane_addressing::addressed_port(Some(reached_authority)) {
        Some(port) => format!("{host}:{port}"),
        None => host,
    }
}

/// The URL a domain's endpoint is announced as, for the request `ctx` is serving: an `https`
/// URL on the host whose leftmost label is `identity` ([`announced_endpoint_authority`]), and
/// nothing after it — the endpoint is the root of that host.
#[must_use]
pub fn announced_endpoint_url(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    identity: &str,
) -> String {
    format!(
        "{DOMAIN_ENDPOINT_SCHEME}://{}",
        announced_endpoint_authority(&announced_authority(ctx), identity)
    )
}

/// A domain's HTTPS endpoint URL: the endpoint announced on the host whose leftmost label is
/// that domain's own id.
#[must_use]
pub fn domain_endpoint_url(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    domain_id: &str,
) -> String {
    announced_endpoint_url(ctx, domain_id)
}

/// The additional endpoint URLs the model describes as "derived from the domain name": the
/// same domain, under the same suffix, on the host whose leftmost label is its NAME rather
/// than its id.
///
/// A domain whose record carries no name has none to derive an endpoint from, so the list
/// is empty — `name` is modelled optional on `Domain`. A domain that carries one publishes
/// exactly that one endpoint.
#[must_use]
pub fn custom_endpoint_urls(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    name: Option<&str>,
) -> Vec<String> {
    name.into_iter()
        .map(|name| announced_endpoint_url(ctx, name))
        .collect()
}

/// `domain` as this service REPORTS it to the request `ctx` is serving: the stored record,
/// with its two endpoint members announced at the address this emulator answers that
/// request on.
///
/// Every operation that answers a `Domain` runs the record through this, so the create, the
/// read and the update announce one set of addresses — and a reader announces the address it
/// was reached on rather than the one the create happened to be reached on.
#[must_use]
pub fn announced_domain(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    domain: &Domain,
) -> Domain {
    Domain {
        domain_endpoint_url: domain_endpoint_url(ctx, &domain.domain_id),
        custom_endpoint_urls: Some(custom_endpoint_urls(ctx, domain.name.as_deref())),
        ..domain.clone()
    }
}

/// Which domain a request addresses the endpoint of, in each of the ways an endpoint of this
/// service is addressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainEndpointRoute<'a> {
    /// The identity the LEFTMOST LABEL of an announced host carries, which is a domain's id
    /// (`domainEndpointUrl`) or its name (a `customEndpointUrls` entry) — the two members
    /// announce one host shape, so the label is resolved against both.
    ByHostLabel(&'a str),
    /// The domain a local path addresses BY ID ([`domain_endpoint_path`]).
    ById(&'a str),
    /// The domain a local path addresses BY NAME ([`domain_name_endpoint_path`]).
    ByName(&'a str),
}

/// The identity the host `host` announces an endpoint of, or `None` for a host that
/// announces none: the leftmost of its labels.
///
/// The inverse of the host [`announced_endpoint_authority`] composes, in the one normal form
/// a host is read in — without userinfo or port
/// (`::service_common::data_plane_addressing::host_only`), without the trailing root dot, and
/// case-insensitively, because those are spellings of the same host rather than other hosts.
///
/// An IP LITERAL names no labels at all — its leading octet is part of an address, not an
/// identity — so an address names no endpoint of a domain, which is also what keeps a request
/// addressed to this emulator's own address from reading as one. Neither does a host of one
/// label, which carries a suffix and no identity in front of it, nor one under a suffix AWS
/// publishes its OWN endpoints at
/// (`::service_common::data_plane_addressing::addresses_an_aws_endpoint`): this emulator never
/// announces a name in a zone AWS serves, so such a host is no address of its minting however
/// its first label reads.
#[must_use]
pub fn domain_endpoint_host_label(host: &str) -> Option<&str> {
    let named = ::service_common::data_plane_addressing::host_only(host).trim_end_matches('.');
    let unbracketed = named.trim_start_matches('[').trim_end_matches(']');
    if unbracketed.is_empty()
        || unbracketed.parse::<::std::net::IpAddr>().is_ok()
        || ::service_common::data_plane_addressing::addresses_an_aws_endpoint(named)
    {
        return None;
    }
    let (label, suffix) = named.split_once('.')?;
    (!label.is_empty() && !suffix.is_empty()).then_some(label)
}

/// The domain endpoint a request addresses — the announced HOST whose first label names a
/// domain, or the local PATH this module also serves one under — and `None` for a request
/// that addresses no endpoint of this service.
///
/// The inverse of the minting above, read through the shared addressing module, so a URL this
/// service published resolves back to the domain behind it. The host a request addressed is
/// read by RFC 9112's rule (`::service_common::data_plane_addressing::addressed_authority`):
/// the authority of an absolute-form target, and the `Host` header otherwise.
///
/// An announced endpoint is ONE page at the ROOT of its host, so a request carrying a path of
/// its own is not that host's endpoint — and the local path form is the same way: a trailing
/// `/` is the same address (that is how a browser normalises a bare path), and nothing BELOW
/// the domain's own segment is this route.
#[must_use]
pub fn addressed_domain_endpoint<'r>(
    uri: &'r str,
    host_header: Option<&'r str>,
) -> Option<DomainEndpointRoute<'r>> {
    if let Some(route) = domain_endpoint_route(uri) {
        return Some(route);
    }
    let path = ::service_common::request_path(uri);
    if !(path.is_empty() || path == "/") {
        return None;
    }
    let addressed =
        ::service_common::data_plane_addressing::addressed_authority(uri, host_header)?;
    domain_endpoint_host_label(addressed).map(DomainEndpointRoute::ByHostLabel)
}

/// The domain endpoint a request `uri` addresses THROUGH THE LOCAL PATH form, and `None` for
/// every other path.
///
/// The reading of [`domain_endpoint_path`] and [`domain_name_endpoint_path`], which is the
/// second address of a domain's page rather than the one a response announces; the whole
/// reading is [`addressed_domain_endpoint`].
#[must_use]
pub fn domain_endpoint_route(uri: &str) -> Option<DomainEndpointRoute<'_>> {
    use ::service_common::data_plane_addressing::{first_segment, EMULATOR_PATH_NAMESPACE};
    let path = ::service_common::request_path(uri);
    let (namespace, rest) = first_segment(path)?;
    if namespace != EMULATOR_PATH_NAMESPACE {
        return None;
    }
    let (plane, rest) = first_segment(rest)?;
    if plane != DOMAIN_PLANE {
        return None;
    }
    let (addressing, rest) = first_segment(rest)?;
    let (identity, below) = first_segment(rest)?;
    if identity.is_empty() || below != "/" {
        return None;
    }
    match addressing {
        DOMAIN_BY_ID_SEGMENT => Some(DomainEndpointRoute::ById(identity)),
        DOMAIN_BY_NAME_SEGMENT => Some(DomainEndpointRoute::ByName(identity)),
        _ => None,
    }
}

/// The media type a domain endpoint is served as: a web page, from this emulator's own
/// origin.
pub const DOMAIN_ENDPOINT_MEDIA_TYPE: &str = "text/html; charset=utf-8";

/// The page a fetch of a domain's endpoint renders: the domain this endpoint belongs to, as
/// the store holds it right now — its id, its name, the Region it was created in and the
/// status it currently reports.
///
/// Every value comes from the stored record, so the page a caller opens cannot disagree with
/// what `GetDomain` reports about the same domain. Those values are stored strings, so each
/// one is written into the document through the `html-escape` crate's escaping rather than
/// inlined.
#[must_use]
pub fn domain_endpoint_page(domain: &Domain) -> String {
    let name = domain.name.clone().unwrap_or_default();
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>CloudWatch Omni — {title}</title>\n</head>\n<body>\n<h1>{title}</h1>\n\
         <dl>\n<dt>Domain</dt><dd>{id}</dd>\n<dt>Region</dt><dd>{region}</dd>\n\
         <dt>Status</dt><dd>{status}</dd>\n</dl>\n</body>\n</html>\n",
        title = escaped(if name.is_empty() { &domain.domain_id } else { &name }),
        id = escaped(&domain.domain_id),
        region = escaped(&domain.region),
        status = escaped(&domain.status),
    )
}

/// The page a fetch of a domain endpoint this emulator holds no domain for renders: a domain
/// that was never created, or one whose deletion has completed.
#[must_use]
pub fn domain_endpoint_not_found_page() -> String {
    "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
     <title>CloudWatch Omni</title>\n</head>\n<body>\n<h1>No such domain</h1>\n\
     </body>\n</html>\n"
        .to_string()
}

/// `value` as text inside an HTML element: the `html-escape` crate's own escaping, so no
/// stored string can open or close an element of the page it is written into.
fn escaped(value: &str) -> String {
    ::html_escape::encode_safe(value).into_owned()
}

impl super::ComAmazonawsCloudwatchomniCloudwatchomnifrontendState {
    /// The domain a published endpoint addresses, or `None` when this emulator holds none
    /// under that identity.
    ///
    /// Every addressing resolves to the same records the modelled reads answer from: the
    /// id-addressed endpoint to the domain held under that id, the name-addressed one to the
    /// domain carrying that name, and the label of an announced HOST to either — the two
    /// announced members carry one host shape, so a label is matched against the ids first and
    /// then against the names. A host is case-insensitive, so the label is compared that way.
    pub fn domain_at_endpoint(&self, route: DomainEndpointRoute<'_>) -> Option<&Domain> {
        match route {
            DomainEndpointRoute::ById(domain_id) => {
                self.domains.get(domain_id).map(|stored| &stored.domain)
            }
            DomainEndpointRoute::ByName(name) => self
                .domains
                .values()
                .map(|stored| &stored.domain)
                .find(|domain| domain.name.as_deref() == Some(name)),
            DomainEndpointRoute::ByHostLabel(label) => self
                .domains
                .values()
                .map(|stored| &stored.domain)
                .find(|domain| domain.domain_id.eq_ignore_ascii_case(label))
                .or_else(|| {
                    self.domains
                        .values()
                        .map(|stored| &stored.domain)
                        .find(|domain| {
                            domain
                                .name
                                .as_deref()
                                .is_some_and(|name| name.eq_ignore_ascii_case(label))
                        })
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a published endpoint path is read back to the domain it names, whichever of the
    /// two addressings minted it, and nothing else is one of this service's endpoints.
    ///
    /// The rows span what the reading generalises over: both addressings, the request forms
    /// a path arrives in (origin form, absolute form, with a query, with a trailing slash),
    /// the identity shapes the two members carry, and the paths that are NOT this route —
    /// another namespace, another plane, another addressing, a path below the endpoint and a
    /// path naming no identity at all.
    #[test]
    fn a_published_endpoint_path_reads_back_to_the_domain_it_names() {
        let by_id = domain_endpoint_path("d-0123456789abcdefghijklmno");
        let by_name = domain_name_endpoint_path("omni-one");
        let rows: &[(&str, String, Option<DomainEndpointRoute<'_>>)] = &[
            (
                "the id-addressed endpoint",
                by_id.clone(),
                Some(DomainEndpointRoute::ById("d-0123456789abcdefghijklmno")),
            ),
            (
                "the name-addressed endpoint",
                by_name.clone(),
                Some(DomainEndpointRoute::ByName("omni-one")),
            ),
            (
                "the shortest modelled id",
                domain_endpoint_path("d-0"),
                Some(DomainEndpointRoute::ById("d-0")),
            ),
            (
                "a trailing slash",
                format!("{by_id}/"),
                Some(DomainEndpointRoute::ById("d-0123456789abcdefghijklmno")),
            ),
            (
                "a query string",
                format!("{by_name}?code=abc"),
                Some(DomainEndpointRoute::ByName("omni-one")),
            ),
            (
                "an absolute-form target",
                format!("https://127.0.0.1:9297{by_id}"),
                Some(DomainEndpointRoute::ById("d-0123456789abcdefghijklmno")),
            ),
            ("a path below the endpoint", format!("{by_id}/auth/code"), None),
            ("the plane with no identity", format!("/_aws/{DOMAIN_PLANE}/domain"), None),
            (
                "another addressing of the plane",
                format!("/_aws/{DOMAIN_PLANE}/space/00000000"),
                None,
            ),
            ("another service's plane", "/_aws/datazone-portal/dzd-1".to_string(), None),
            ("another namespace", format!("/{DOMAIN_PLANE}/domain/d-0"), None),
            ("a modelled route", "/service/CloudWatchOmniFrontend/operation/GetDomain".to_string(), None),
            ("the root", "/".to_string(), None),
        ];
        for (case, uri, expected) in rows {
            let actual = domain_endpoint_route(uri);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?} for {uri:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the identity an announced endpoint carries is the LEFTMOST LABEL of the host the
    /// request addressed, and a host that carries no label in front of a suffix — an address
    /// literal, a single label — announces no endpoint.
    ///
    /// The rows span what the reading generalises over: the two identity shapes the members
    /// carry (a domain id, a domain name), the spellings of one host (a port, userinfo, a
    /// trailing root dot, mixed case), the suffix shapes an emulator is reached under (the
    /// loopback zone, a DNS name, the unserved base), and the hosts that announce nothing —
    /// IPv4 and IPv6 literals, a bracketed literal with a port, a bare suffix and an empty
    /// host.
    #[test]
    fn an_announced_host_names_the_domain_in_its_first_label() {
        let rows: &[(&str, &str, Option<&str>)] = &[
            ("an id-labelled host", "d-0123456789abcdefghijklmno.localhost:9297", Some("d-0123456789abcdefghijklmno")),
            ("a name-labelled host", "omni-one.localhost:9297", Some("omni-one")),
            ("the shortest modelled id", "d-0.localhost", Some("d-0")),
            ("a host carrying userinfo", "visitor@omni-one.localhost:9297", Some("omni-one")),
            ("a rooted host", "omni-one.localhost.", Some("omni-one")),
            ("a host in mixed case", "Omni-One.LocalHost:9297", Some("Omni-One")),
            ("a DNS suffix", "d-0.emulator.invalid", Some("d-0")),
            ("a deeper suffix", "omni-one.endpoint.example.test:8443", Some("omni-one")),
            ("an IPv4 literal", "127.0.0.1:9297", None),
            ("another IPv4 literal", "192.0.2.10", None),
            ("a bracketed IPv6 literal", "[::1]:9297", None),
            ("an IPv6 literal with no brackets", "::1", None),
            ("the suffix alone", "localhost:9297", None),
            ("a host under a suffix AWS publishes", "omni-one.cloudwatch-omni.api.aws", None),
            ("one of AWS's own service endpoints", "d-0.cloudwatch.us-west-2.amazonaws.com", None),
            ("no host at all", "", None),
        ];
        for (case, host, expected) in rows {
            let actual = domain_endpoint_host_label(host);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?} for {host:?}, got {actual:?}"
            );
        }

        // And the whole reading of a REQUEST: the host form at the root of the host, the
        // local path form, and the requests that address no endpoint of this service.
        let request_rows: &[(&str, &str, Option<&str>, Option<DomainEndpointRoute<'_>>)] = &[
            (
                "a fetch of an announced host",
                "/",
                Some("d-omni.localhost:9297"),
                Some(DomainEndpointRoute::ByHostLabel("d-omni")),
            ),
            (
                "the same fetch in absolute form",
                "https://omni-one.localhost:9297/",
                None,
                Some(DomainEndpointRoute::ByHostLabel("omni-one")),
            ),
            (
                "an absolute-form target with no path at all",
                "https://d-omni.localhost:9297",
                None,
                Some(DomainEndpointRoute::ByHostLabel("d-omni")),
            ),
            (
                "the local path form on an announced host",
                &domain_endpoint_path("d-omni"),
                Some("d-omni.localhost:9297"),
                Some(DomainEndpointRoute::ById("d-omni")),
            ),
            (
                "a path below an announced host's root",
                "/auth/code",
                Some("d-omni.localhost:9297"),
                None,
            ),
            (
                "a modelled call on an announced host",
                "/service/CloudWatchOmniFrontend/operation/GetDomain",
                Some("d-omni.localhost:9297"),
                None,
            ),
            ("a fetch of the emulator's own address", "/", Some("127.0.0.1:9297"), None),
            (
                "a fetch of a host AWS publishes its own endpoints at",
                "/",
                Some("d-omni.cloudwatch-omni.api.aws"),
                None,
            ),
            ("a fetch naming no host", "/", None, None),
        ];
        for (case, uri, host, expected) in request_rows {
            let actual = addressed_domain_endpoint(uri, *host);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?} for {uri:?} on {host:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the page a fetch of an endpoint renders states the domain the store holds, and
    /// writes every stored value through HTML escaping.
    #[test]
    fn the_endpoint_page_states_the_stored_domain() {
        let mut domain = Domain {
            domain_id: "d-omni".to_string(),
            domain_arn: "arn:aws:cloudwatch:us-west-2:000000000000:domain/d-omni".to_string(),
            name: Some("omni-one".to_string()),
            identity_providers: vec![super::super::IDENTITY_PROVIDER_IDC.to_string()],
            identity_provider_configuration: None,
            domain_endpoint_url: String::new(),
            custom_endpoint_urls: None,
            identity_center_application_arn: None,
            region: "us-west-2".to_string(),
            created_at: ::aws_smithy_types::DateTime::from_secs(1_700_000_000),
            updated_at: ::aws_smithy_types::DateTime::from_secs(1_700_000_000),
            status: super::super::DOMAIN_STATUS_ACTIVE.to_string(),
        };
        let page = domain_endpoint_page(&domain);
        for expected in ["d-omni", "omni-one", "us-west-2", super::super::DOMAIN_STATUS_ACTIVE] {
            assert!(
                page.contains(expected),
                "case \"a stored domain\": expected the page to state {expected:?}, got {page:?}"
            );
        }

        // A value carrying markup is escaped rather than inlined: no stored string can open
        // an element of the page it is written into.
        domain.region = "<script>x</script>".to_string();
        let page = domain_endpoint_page(&domain);
        assert!(
            !page.contains("<script>"),
            "case \"a value carrying markup\": expected it escaped, got {page:?}"
        );
    }
}
