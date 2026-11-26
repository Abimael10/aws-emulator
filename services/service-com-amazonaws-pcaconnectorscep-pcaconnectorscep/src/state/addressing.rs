// SPDX-License-Identifier: CC0-1.0

//! How a client ADDRESSES a connector's SCEP plane, and how an address is read back.
//!
//! One module for both directions, so the address this service publishes and the address it
//! claims cannot drift apart:
//!
//! * the hosts and URLs a connector is published at — the SCEP host AWS publishes
//!   ([`connector_endpoint`]), the emulator's own endpoint ([`local_connector_endpoint`]) and
//!   the choice between them ([`announced_connector_endpoint`]);
//! * the reverse readings of those addresses, each the inverse of the function that builds it
//!   ([`parse_connector_plane_path`], [`connector_addressed_by_host`]), answering which
//!   connector an address names and over which access path ([`AddressedConnector`],
//!   [`AddressedAccessPath`]);
//! * and the match of a whole address against the stored record, which is what makes a
//!   connector answer at the address it was published at and at no other
//!   ([`StoredConnector::is_published_at`],
//!   [`ComAmazonawsPcaconnectorscepPcaconnectorscepState::connector_addressed`]).
//!
//! A reverse reading parses an address an untrusted client chose, so every step of it is
//! total: the host is read with `strip_suffix` / `split_once` / `strip_prefix` and the shared
//! path reading (`service_common::data_plane_addressing`), each step answering `None` for a
//! value of another shape. Nothing is indexed, sliced or unwrapped, and no step can panic.

use super::*;

/// DNS suffix of a host reachable over the public internet, which is where a connector
/// created without a VPC endpoint id serves.
pub(super) const PUBLIC_DNS_SUFFIX: &str = "amazonaws.com";

/// DNS suffix of a host reachable only through an interface VPC endpoint — the `vpce.`
/// label AWS PrivateLink puts in front of [`PUBLIC_DNS_SUFFIX`] for an endpoint-specific
/// DNS name. A connector whose create named a VPC endpoint serves only there.
pub(super) const PRIVATE_DNS_SUFFIX: &str = "vpce.amazonaws.com";

/// The host label every connector's SCEP host carries in front of this service's own
/// regional domain, and the segment that names the same plane in the path the surface is
/// published at on the emulator's own endpoint ([`local_connector_endpoint`]). One
/// spelling, so the address announced and the address claimed cannot drift.
pub(super) const SCEP_HOST_LABEL: &str = "scep";

/// The connector's HTTPS SCEP URL on the host AWS publishes it under.
///
/// The model states only that it is an HTTPS SCEP URL, so the host is derived from the
/// connector's own id and the Region the request was signed for: stable across reads,
/// distinct for two connectors, and carrying no clock or request id.
///
/// The access path the create chose is part of that derivation, because it is what makes the
/// URL reachable: with no VPC endpoint id the connector is "accessible over the public
/// internet" (the `VpcEndpointId` member's own doc) and the host sits under the public DNS
/// suffix; with one, the connector is "accessible only through that specific VPC endpoint",
/// so the host is qualified by that endpoint's id and sits under the PrivateLink suffix
/// instead. Two connectors that differ only in that choice therefore report different hosts.
///
/// This is the address for a caller that resolves AWS's own hostnames; what this service
/// ANNOUNCES to the caller in front of it is [`announced_connector_endpoint`].
pub fn connector_endpoint(
    region: &str,
    connector_id: &str,
    vpc_endpoint_id: Option<&str>,
) -> String {
    match vpc_endpoint_id {
        Some(vpc_endpoint_id) => format!(
            "https://{connector_id}.{SCEP_HOST_LABEL}.{vpc_endpoint_id}.{ARN_SERVICE}.{region}.{PRIVATE_DNS_SUFFIX}"
        ),
        None => format!(
            "https://{connector_id}.{SCEP_HOST_LABEL}.{ARN_SERVICE}.{region}.{PUBLIC_DNS_SUFFIX}"
        ),
    }
}

/// One connector's SCEP URL on the EMULATOR'S OWN endpoint:
/// `https://<authority>/_aws/scep/<region>/<connector-id>`.
///
/// It carries what the published host carries, with the Region and the connector's id moved
/// out of the host and into the PATH: a client on the machine this emulator runs on resolves
/// no `amazonaws.com` name, so the only address it can dial is the endpoint it already
/// reached, and the plane is named by [`SCEP_HOST_LABEL`] behind the reserved path segment
/// every data plane this emulator serves on its own endpoint sits under
/// (`service_common::data_plane_addressing`) — a path under that segment naming another
/// plane is never read as this one's.
pub fn local_connector_endpoint(authority: &str, region: &str, connector_id: &str) -> String {
    format!(
        "https://{authority}/{}/{SCEP_HOST_LABEL}/{region}/{connector_id}",
        ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE
    )
}

/// The `Endpoint` this service ANNOUNCES for a connector, resolved once when the connector
/// is created and stored, so every later read reports the same value.
///
/// The member exists for a client to DIAL — it is "the connector's HTTPS public SCEP URL",
/// the address a SCEP-enabled device or an MDM system is configured with — so the address
/// announced has to be one this emulator itself answers on:
///
/// * a request that addressed the EMULATOR'S OWN endpoint
///   ([`reached_emulator_endpoint`]) comes from a client that resolves nothing else, so the
///   connector is announced on that same endpoint ([`local_connector_endpoint`]), the
///   addressing [`crate::service::scep`] serves;
/// * a connector created through any other request on an emulator that IS served on a port
///   ([`served_endpoint_authority`]) is announced on that port, the one address this process
///   answers on, so a connector minted by an in-process call of another service announces an
///   address its caller can reach too;
/// * a request that addressed one of AWS's own hosts on an emulator served on no port at all
///   comes from a client that resolves them, so the connector is announced on the host
///   [`connector_endpoint`] builds — which the same plane claims host-style.
pub fn announced_connector_endpoint(
    parts: &::http::request::Parts,
    served_base: &str,
    region: &str,
    connector_id: &str,
    vpc_endpoint_id: Option<&str>,
) -> String {
    match reached_emulator_endpoint(parts).or_else(|| served_endpoint_authority(served_base)) {
        Some(authority) => local_connector_endpoint(&authority, region, connector_id),
        None => connector_endpoint(region, connector_id, vpc_endpoint_id),
    }
}

/// The EMULATOR'S OWN endpoint this request reached — an address or a loopback name, which
/// is what a client configured with one local endpoint URL sends — and `None` for a request
/// whose host names a service, which is a client that resolves AWS's own hosts.
///
/// The tree's one reading of that question
/// (`service_common::data_plane_addressing::reached_emulator_endpoint`), so this service
/// writes no second rule for which host is the emulator's own; it reads the host off the
/// header or off an absolute-form URI, as both carry it.
pub(super) fn reached_emulator_endpoint(parts: &::http::request::Parts) -> Option<String> {
    ::service_common::data_plane_addressing::reached_emulator_endpoint(
        &parts.uri.to_string(),
        parts
            .headers
            .get(::http::header::HOST)
            .and_then(|value| value.to_str().ok()),
    )
}

/// The `host[:port]` this emulator is SERVED at, read off the base URL
/// `OperationContext::endpoint` reports ("the base URL a later request addresses… Callers
/// send normally signed requests to it"), or `None` for an emulator that is served on no
/// port at all — which is what its unserved base names
/// (`smithy_server_runtime::UNSERVED_URL_BASE`, a host RFC 6761 reserves so that nothing
/// resolves it).
///
/// Only a value SHAPED like a host is taken
/// (`service_common::data_plane_addressing::is_host_shaped`), so nothing that is not an
/// address reaches a URL this service announces.
pub(super) fn served_endpoint_authority(served_base: &str) -> Option<String> {
    if served_base == ::smithy_server_runtime::UNSERVED_URL_BASE {
        return None;
    }
    let authority =
        ::service_common::data_plane_addressing::url_authority(served_base).trim_end_matches('/');
    ::service_common::data_plane_addressing::is_host_shaped(authority)
        .then(|| authority.to_string())
}

/// Which connector's SCEP plane a request addresses: the Region it lives in and its own id,
/// which together name exactly one connector ([`connector_arn`]), plus the access path the
/// address itself took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressedConnector {
    pub region: String,
    pub connector_id: String,
    /// The access path this address reaches the plane over, which a connector only serves at
    /// when it is the one it was published under ([`StoredConnector::is_published_at`]).
    pub access_path: AddressedAccessPath,
}

impl AddressedConnector {
    /// The ARN of the connector this address names — the key its record is stored under.
    pub fn connector_arn(&self) -> String {
        connector_arn(&self.region, &self.connector_id)
    }
}

/// The access path an address takes to this service's SCEP plane — the half of a published
/// address that says HOW the plane is reached, as opposed to which connector it names.
///
/// A connector is reachable at the access path its create chose and at no other: with no VPC
/// endpoint id it is "accessible over the public internet" and with one "accessible only
/// through that specific VPC endpoint" (the `VpcEndpointId` member's own doc). So the address
/// a request used carries that choice, and the record says which choice was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressedAccessPath {
    /// The EMULATOR'S OWN endpoint ([`local_connector_endpoint`]): the one address a client
    /// that resolves no `amazonaws.com` name can dial, so it expresses no access path of its
    /// own and every connector announced there is reached there.
    EmulatorEndpoint,
    /// The public SCEP host ([`connector_endpoint`] with no VPC endpoint id), which is where a
    /// connector accessible over the public internet serves.
    Public,
    /// A PrivateLink SCEP host qualified by this VPC endpoint id, which is where a connector
    /// accessible only through that specific VPC endpoint serves.
    VpcEndpoint(String),
}

/// Whether `path` addresses this service's SCEP plane on the emulator's own endpoint at all
/// — the reserved namespace segment and the segment that names this surface, whatever
/// follows them.
///
/// It is what the service CLAIMS such a request on: a path on this plane is this service's
/// to answer however the rest of it reads, so a client holding a URL this service published
/// is never answered by the front door's "unknown service".
pub fn addresses_connector_plane(path: &str) -> bool {
    connector_plane_path(path).is_some()
}

/// The connector a path on the EMULATOR'S own endpoint names, or `None` when the path is not
/// one [`local_connector_endpoint`] mints there.
///
/// The inverse of that function, so what is recognised is exactly what is published: the
/// Region segment has to be one an ARN of this service can carry ([`is_region_shaped`]) and
/// the id segment has to be the last one, bar an empty trailing segment — a URL a client
/// pasted with its trailing slash.
pub fn parse_connector_plane_path(path: &str) -> Option<AddressedConnector> {
    use ::service_common::data_plane_addressing::first_segment;
    let (region, rest) = first_segment(connector_plane_path(path)?)?;
    if !is_region_shaped(region) {
        return None;
    }
    let (connector_id, rest) = first_segment(rest)?;
    if connector_id.is_empty() || rest != "/" {
        return None;
    }
    Some(AddressedConnector {
        region: region.to_string(),
        connector_id: connector_id.to_string(),
        access_path: AddressedAccessPath::EmulatorEndpoint,
    })
}

/// The path BENEATH this service's SCEP plane prefix, or `None` for a path that is not on
/// it: the one reading of that prefix, by exact segment equality.
pub(super) fn connector_plane_path(path: &str) -> Option<&str> {
    use ::service_common::data_plane_addressing::{first_segment, EMULATOR_PATH_NAMESPACE};
    let (namespace, rest) = first_segment(path)?;
    if namespace != EMULATOR_PATH_NAMESPACE {
        return None;
    }
    let (plane, rest) = first_segment(rest)?;
    (plane == SCEP_HOST_LABEL).then_some(rest)
}

/// The connector a published SCEP HOST names, or `None` when the host is not one
/// [`connector_endpoint`] builds.
///
/// The inverse of that function — both of its forms, public and PrivateLink — so a client
/// that DOES resolve AWS's own hostnames reaches the same plane at the same address this
/// service published to it. The Region label has to be one an ARN of this service can carry,
/// and the first label is the connector's own id.
///
/// The access path the host form takes is read out with it ([`AddressedAccessPath`]) rather
/// than discarded, because a host of one form names no connector published under the other:
/// which of the two a given connector is published at is its stored access path, so the
/// inverse is completed by [`StoredConnector::is_published_at`] against that record.
pub fn connector_addressed_by_host(host: &str) -> Option<AddressedConnector> {
    let host = ::service_common::data_plane_addressing::host_only(host).to_ascii_lowercase();
    // The PrivateLink suffix is tried FIRST because it ENDS with the public one, so the
    // public strip would otherwise leave the `vpce` label on the Region. Which suffix the
    // host carries is half of the access path it names, so it is carried along rather than
    // forgotten: the PrivateLink suffix belongs to the endpoint-qualified form and the public
    // one to the form with no endpoint in it.
    let (body, private_link) = match host.strip_suffix(&format!(".{PRIVATE_DNS_SUFFIX}")) {
        Some(body) => (body, true),
        None => (host.strip_suffix(&format!(".{PUBLIC_DNS_SUFFIX}"))?, false),
    };
    let (head, region) = body.rsplit_once('.')?;
    let (head, service) = head.rsplit_once('.')?;
    if service != ARN_SERVICE || !is_region_shaped(region) {
        return None;
    }
    // What is left is the connector's own id and the plane's label, which the PrivateLink
    // form follows with the VPC endpoint id the host is reached through.
    let (connector_id, labels) = head.split_once('.')?;
    let access_path = match labels.strip_prefix(SCEP_HOST_LABEL)? {
        // The public form: the plane's label and nothing else, under the public suffix.
        "" if !private_link => AddressedAccessPath::Public,
        // The PrivateLink form: one more label, the VPC endpoint id, under the PrivateLink
        // suffix.
        rest if private_link => {
            let vpc_endpoint_id = rest
                .strip_prefix('.')
                .filter(|vpc_endpoint_id| {
                    !vpc_endpoint_id.is_empty() && !vpc_endpoint_id.contains('.')
                })?;
            AddressedAccessPath::VpcEndpoint(vpc_endpoint_id.to_string())
        }
        // Either suffix carrying the labels of the other form is a host this service builds
        // for no connector.
        _ => return None,
    };
    (!connector_id.is_empty()).then(|| AddressedConnector {
        region: region.to_string(),
        connector_id: connector_id.to_string(),
        access_path,
    })
}

impl StoredConnector {
    /// Whether this connector is PUBLISHED at an address that took `access_path` — the one
    /// access path it serves on.
    ///
    /// A create with no VPC endpoint id makes the connector "accessible over the public
    /// internet" and one with it "accessible only through that specific VPC endpoint" (the
    /// `VpcEndpointId` member's own doc), so the access path the record stores is the only one
    /// its plane answers at: a public connector is not reached through any VPC endpoint, and
    /// one restricted to a VPC endpoint is reached neither over the public internet nor
    /// through another VPC endpoint. The emulator's own endpoint carries no access path at all
    /// ([`AddressedAccessPath::EmulatorEndpoint`]) — it is the single address a local client
    /// can dial, and the address every connector is announced on there.
    ///
    /// A VPC endpoint id is compared without regard to letter case, because it reaches this
    /// service as a DNS label of the address and DNS names are case-insensitive.
    pub fn is_published_at(&self, access_path: &AddressedAccessPath) -> bool {
        match access_path {
            AddressedAccessPath::EmulatorEndpoint => true,
            AddressedAccessPath::Public => self.vpc_endpoint_id.is_none(),
            AddressedAccessPath::VpcEndpoint(addressed) => self
                .vpc_endpoint_id
                .as_deref()
                .is_some_and(|published| published.eq_ignore_ascii_case(addressed)),
        }
    }
}

impl ComAmazonawsPcaconnectorscepPcaconnectorscepState {
    /// The connector PUBLISHED at the address `addressed` names, or `None` when no connector
    /// of this service answers there.
    ///
    /// A published address names a connector only when BOTH halves of it agree with the
    /// record: the Region and id half resolves one connector ([`Self::connector`]), and the
    /// access-path half is the one that connector was published under
    /// ([`StoredConnector::is_published_at`]). This is the one reading the data plane resolves
    /// an address through, so an address this service never published for a connector resolves
    /// to nothing rather than to that connector.
    pub fn connector_addressed(&self, addressed: &AddressedConnector) -> Option<&StoredConnector> {
        self.connector(&addressed.region, &addressed.connector_arn())
            .filter(|connector| connector.is_published_at(&addressed.access_path))
    }
}
