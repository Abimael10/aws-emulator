// SPDX-License-Identifier: CC0-1.0

//! The ADDRESSES a domain is announced at — `Domain.domainEndpointUrl` and every
//! `Domain.customEndpointUrls` entry — the route that answers there, and the page a fetch
//! of one of them renders.
//!
//! `domainEndpointUrl` is "The HTTPS endpoint URL for accessing the domain" and
//! `customEndpointUrls` is "Additional endpoint URLs derived from the domain name"
//! (`smithy_query(query="shape",shape="Domain")`). Neither is a label a caller reads back
//! and puts away: both are URLs a browser, a probe or a deep-link flow OPENS. So the only
//! AUTHORITY either may carry is one this emulator answers on — the `host[:port]` the
//! request was reached at ([`announced_authority`]), which for a client that resolves nothing
//! but the endpoint it is configured with is the endpoint it is already talking to.
//!
//! That authority is the whole of the address this emulator has to offer. A listener of this
//! emulator answers for the address it is bound on and for the loopback name, and a name this
//! emulator invented under either of them — an identity label in front of the host the request
//! arrived at — resolves nowhere near this process and is refused as a `Host` even when a
//! resolver does answer it, so announcing one hands a caller an address that reaches nothing.
//! A name in a zone AWS publishes its own endpoints in is worse still: a client that resolves
//! it leaves the emulated environment altogether.
//!
//! WHICH domain an endpoint belongs to is therefore carried where this emulator can carry it,
//! in the PATH ([`domain_endpoint_path`], [`domain_name_endpoint_path`]) — behind the reserved
//! namespace segment every emulator-served plane sits under
//! (`::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE`), the segment that
//! names this service's own plane, and the segment that says which addressing follows: the
//! domain's id for the endpoint "for accessing the domain" and the domain's name for each
//! endpoint "derived from the domain name". A client recovers the domain an endpoint belongs
//! to by reading that identity segment, and derives a name endpoint from a name, so the two
//! members stay one domain's two addresses under one shared authority.
//!
//! The members are composed at PROJECTION time, per request ([`announced_domain`]), and the
//! composition is never written by a read: a domain read through one address is never
//! answered another address's, and two deployments of this emulator answer on two
//! addresses, so they announce two sets.
//!
//! One function mints each URL and one reads an address back ([`domain_endpoint_route`]), and
//! `crate::service` SERVES exactly those addresses from the stored domain, so what a response
//! advertises and what this emulator answers cannot drift apart.
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

/// The request path the domain `domain_id` is served at, on this emulator's own endpoint —
/// the path the announced `domainEndpointUrl` carries ([`domain_endpoint_url`]).
///
/// The identity sits in the path because the host cannot carry it: this emulator answers only
/// on the address a request reached it at (see the module docs).
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

/// The request path the domain NAMED `name` is served at, on this emulator's own endpoint —
/// the name-addressed counterpart of [`domain_endpoint_path`], which is the path each
/// announced `customEndpointUrls` entry carries ([`custom_endpoint_urls`]).
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

/// The URL an endpoint served at `path` is announced as, for the request `ctx` is serving: an
/// `https` URL on the authority this emulator answered that request at
/// ([`announced_authority`]), then the path it serves the endpoint under.
///
/// The authority carries the PORT the request arrived on, so a client dials the socket this
/// emulator answers at rather than the implicit HTTPS port nothing serves.
#[must_use]
pub fn announced_endpoint_url(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    path: &str,
) -> String {
    format!(
        "{DOMAIN_ENDPOINT_SCHEME}://{}{path}",
        announced_authority(ctx)
    )
}

/// A domain's HTTPS endpoint URL: the endpoint this emulator serves that domain's id at
/// ([`domain_endpoint_path`]), on the authority the request reached it on.
#[must_use]
pub fn domain_endpoint_url(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    domain_id: &str,
) -> String {
    announced_endpoint_url(ctx, &domain_endpoint_path(domain_id))
}

/// The additional endpoint URLs the model describes as "derived from the domain name": the
/// same domain on the same authority, addressed by its NAME rather than by its id
/// ([`domain_name_endpoint_path`]).
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
        .map(|name| announced_endpoint_url(ctx, &domain_name_endpoint_path(name)))
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
    /// The domain the announced `domainEndpointUrl`'s path addresses BY ID
    /// ([`domain_endpoint_path`]).
    ById(&'a str),
    /// The domain a `customEndpointUrls` entry's path addresses BY NAME
    /// ([`domain_name_endpoint_path`]).
    ByName(&'a str),
}

/// The domain endpoint a request `uri` addresses, and `None` for a request that addresses no
/// endpoint of this service.
///
/// The inverse of the minting above, read through the shared addressing module, so a URL this
/// service published resolves back to the domain behind it: the reserved namespace segment,
/// this service's own plane, the addressing and the identity it names.
///
/// An announced endpoint is ONE page: a trailing `/` is the same address (that is how a
/// browser normalises a bare path), and nothing BELOW the domain's own segment is this route.
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
    /// Both addressings resolve to the same records the modelled reads answer from: the
    /// id-addressed endpoint to the domain held under that id, the name-addressed one to the
    /// domain carrying that name.
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

    /// RULE: an endpoint of this service is addressed by the PATH it is served under, so a
    /// request that carries a resource identity in the HOST instead addresses none — a name
    /// under the address this emulator is reached at is a name nothing here answers for.
    ///
    /// The rows span the host shapes such a request names: an identity label under the
    /// loopback zone, under a DNS name an operator serves the emulator at and under a zone AWS
    /// publishes its own endpoints in, the emulator's own address, and the same hosts carrying
    /// the one path that IS this service's.
    #[test]
    fn a_resource_identity_in_the_host_addresses_no_endpoint_of_this_service() {
        let by_id = domain_endpoint_path("d-omni");
        let rows: &[(&str, String, Option<DomainEndpointRoute<'_>>)] = &[
            (
                "an identity label under the loopback zone",
                "https://d-omni.localhost:9297/".to_string(),
                None,
            ),
            (
                "an identity label with no path at all",
                "https://omni-one.localhost:9297".to_string(),
                None,
            ),
            (
                "an identity label under a DNS name",
                "https://d-omni.emulator.invalid:8443/".to_string(),
                None,
            ),
            (
                "an identity label in a zone AWS publishes",
                "https://d-omni.cloudwatch-omni.global.app.aws/".to_string(),
                None,
            ),
            (
                "the emulator's own address at its root",
                "https://127.0.0.1:9297/".to_string(),
                None,
            ),
            (
                "the served path on the emulator's own address",
                format!("https://127.0.0.1:9297{by_id}"),
                Some(DomainEndpointRoute::ById("d-omni")),
            ),
            (
                "the served path on a host carrying an identity label",
                format!("https://d-omni.localhost:9297{by_id}"),
                Some(DomainEndpointRoute::ById("d-omni")),
            ),
        ];
        for (case, uri, expected) in rows {
            let actual = domain_endpoint_route(uri);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?} for {uri:?}, got {actual:?}"
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
