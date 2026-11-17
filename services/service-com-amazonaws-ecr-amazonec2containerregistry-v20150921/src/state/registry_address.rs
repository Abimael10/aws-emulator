// SPDX-License-Identifier: CC0-1.0

//! The registry ADDRESS a repository is pushed and pulled at, and the zone an address this
//! service ANNOUNCES for a client to DIAL is named in.
//!
//! # What such an address names
//!
//! An Amazon ECR registry is ONE account's in ONE Region ("the region of the repository,
//! Amazon Web Services account ID of the repository owner, repository namespace, and
//! repository name", `Repository$repositoryArn`), so the same repository name names two
//! different repositories in two Regions. Every address this service announces for one
//! therefore names the registry that holds it, in the layout
//! `AuthorizationData$proxyEndpoint` spells out ("The Amazon ECR registry URL format is
//! `https://aws_account_id.dkr.ecr.region.amazonaws.com`"): the owning account's id, the
//! two fixed labels, the registry's Region, then the zone the registry is served in
//! ([`announced_registry_host`]). Two registries are then never one host, and the account
//! and Region an address addresses can be read back off it ([`registry_host_parts`]) —
//! which is what a client that derives a `docker login` target, a task definition's `image`
//! or a cross-account check from the address it was handed does with it.
//!
//! `Repository$repositoryUri` ("The URI for the repository. You can use this URI for
//! container image `push` and `pull` operations") is that host followed by `/` and the
//! repository's own name ([`repository_uri`]), and `AuthorizationData$proxyEndpoint` ("The
//! registry URL to use for this authorization token in a `docker login` command") is the
//! same host behind a scheme ([`registry_login_url`]), so a client logs in at the host it
//! pushes to, exactly as it does on AWS.
//!
//! # Which zone it is announced in
//!
//! The one thing that cannot carry over from AWS is the ZONE. These values are not labels a
//! caller reads back: they are addresses a container client DIALS, and AWS's own
//! `amazonaws.com` registry host resolves to nothing on the machine this emulator runs on,
//! so a URI naming it sends every push and pull out of the emulator entirely and the
//! repository can never hold an image. The registry's labels are therefore announced in the
//! zone this emulator ANSWERS in, with the port it is reached on
//! ([`announced_registry_host`], built on the tree's one reading of that zone,
//! `service_common::data_plane_addressing::announced_zone_of_authority`) — the loopback zone
//! for an emulator reached by address, which RFC 6761 reserves together with every name
//! under it for the loopback interface, and the operator's own name where they serve it at
//! one. A zone AWS publishes its own endpoints in is never announced, whatever host a request
//! carries; a caller that reached this emulator at a registry host in a zone of its own keeps
//! that zone, so an address this registry announces is announced the same wherever a client
//! dialled it from.
//!
//! This registry's `/v2` data plane claims a request whatever host it arrived on
//! (`crate::service::registry_v2`) and reads which Region's registry the client dialled off
//! that host ([`dialled_registry_region`]), so a push or pull written on an announced
//! address reaches the very repository the address names.

/// The endpoint this emulator answers THIS caller at, which is the zone and port every
/// registry address announced to it is named in ([`announced_registry_host`]).
///
/// Two endpoints can satisfy that, and the tree's one reading of the question picks between
/// them (`service_common::data_plane_addressing::reached_emulator_endpoint`):
///
/// - the endpoint the CALLER reached this emulator on, when that endpoint names no
///   service — the emulator's own socket, a loopback name, and a registry host itself,
///   whose first label is an account id. `crate::service`'s registry data plane claims a
///   `/v2` request exactly when its endpoint names no service, so such a caller is named
///   an address in the zone it is already talking to and reaches the registry there;
/// - otherwise the address this emulator is served at: a caller that came in on some
///   service's endpoint host (an SDK pointed at `api.ecr.<region>.amazonaws.com`) would be
///   handed an address a registry client cannot use, because that host routes to a service
///   and no registry request addressed to it reaches this plane — and so would a caller that
///   came in on a host inside a zone AWS publishes its OWN endpoints in
///   (`addresses_an_aws_endpoint`), which is not a zone this emulator announces a name in at
///   all: a name there resolves nowhere near this process. Taking the served address WHOLE,
///   zone and port together, is what keeps the announced host dialable.
pub fn announced_registry_endpoint(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> String {
    let parts = ctx.parts();
    ::service_common::data_plane_addressing::reached_emulator_endpoint(
        &parts.uri.to_string(),
        parts
            .headers
            .get(::http::header::HOST)
            .and_then(|value| value.to_str().ok()),
    )
    .filter(|reached| {
        !::service_common::data_plane_addressing::addresses_an_aws_endpoint(reached)
    })
    .unwrap_or_else(|| {
        ::service_common::data_plane_addressing::emulator_endpoint_authority(ctx)
    })
}

/// The Region of the registry a request to one of this registry's own DATA-PLANE endpoints
/// DIALLED: the Region named by the registry host it was addressed to, read through the one
/// reading of that host this service states ([`registry_host_parts`], held to the account
/// this registry serves by `super::registry_pull::host_region`), and `None` for an endpoint
/// that names no registry — this emulator's own socket, which serves every Region's
/// registry.
///
/// A data-plane request carries no SigV4 credential scope of its own — a registry client
/// presents the `docker login` credential as HTTP Basic authorization — so the endpoint it
/// dialled is what says which Region's registry it addresses, exactly as an image reference
/// does for an in-process pull (`super::registry_pull::parse_reference`). Where the endpoint
/// names none, the credential presented does: a `docker login` credential is obtained per
/// registry (`super::registry_v2::RegistryCredential::region`).
///
/// `authority` is the endpoint the request arrived on, host and optional port.
pub fn dialled_registry_region(authority: &str) -> Option<String> {
    super::registry_pull::host_region(authority)
}

/// `<registry host>/<name>` — the URI a client pushes the repository `name` to and pulls it
/// from, in the registry `registry_host` names ([`announced_registry_host`]).
///
/// The repository's own name is appended to the host exactly as AWS appends it to a registry
/// host, so a client composes the same tag, digest or registry v2 path on what it was
/// handed, and the path after the first `/` is exactly the repository name.
pub fn repository_uri(registry_host: &str, name: &str) -> String {
    format!("{registry_host}/{name}")
}

/// The two fixed labels every Amazon ECR registry host carries between the owning account's
/// id and the registry's Region, as the registry URL format
/// `AuthorizationData$proxyEndpoint` documents spells them
/// (`https://aws_account_id.dkr.ecr.region.amazonaws.com`).
///
/// The one composition of that host ([`registry_host_in`]) and the one reading of it
/// ([`registry_host_parts`]) are both written on them, so a host this registry announces
/// parses back as a reference to the very registry it names.
pub const REGISTRY_HOST_LABELS: [&str; 2] = ["dkr", "ecr"];

/// `<registryId>.dkr.ecr.<region>.<zone>` — the host of the registry account `registry_id`
/// holds in `region`, as this emulator ANNOUNCES it to the caller it is answering: the
/// registry's own labels in the zone this emulator answers that caller in, carrying the port
/// it is reached on (see the module docs).
///
/// `announced_endpoint` is the endpoint this emulator answers that caller at
/// ([`announced_registry_endpoint`]). Its ZONE is where the labels are announced — the
/// tree's one reading of that question
/// (`service_common::data_plane_addressing::announced_zone_of_authority`), which answers the
/// loopback zone for an endpoint named by address or named in a zone AWS publishes its own
/// endpoints in, and the operator's own name for one they serve at a name — except for an
/// endpoint that is a registry host in a zone of its own already, whose zone is kept so that
/// re-announcing an address a client dialled yields the same host rather than a deeper one.
pub fn announced_registry_host(
    announced_endpoint: &str,
    registry_id: &str,
    region: &str,
) -> String {
    let zone = announced_registry_zone(announced_endpoint);
    match ::service_common::data_plane_addressing::addressed_port(Some(announced_endpoint)) {
        Some(port) => registry_host_in(registry_id, region, &format!("{zone}:{port}")),
        None => registry_host_in(registry_id, region, &zone),
    }
}

/// The zone the registry labels are announced in for a caller answered at
/// `announced_endpoint`, without its port: that endpoint's own zone, read through the tree's
/// one reading of it.
///
/// An endpoint that is ITSELF a registry host keeps the zone that host sits in rather than
/// being read as a zone to announce one more registry host inside of — so re-announcing an
/// address a client dialled yields the same host. The endpoint is one this emulator answers
/// at ([`announced_registry_endpoint`]), never a host in a zone AWS publishes its own
/// endpoints in, so that carve-out cannot echo AWS's zone.
fn announced_registry_zone(announced_endpoint: &str) -> String {
    match registry_host_parts(announced_endpoint) {
        Some(parts) => parts.zone.to_ascii_lowercase(),
        None => ::service_common::data_plane_addressing::announced_zone_of_authority(
            announced_endpoint,
        ),
    }
}

/// `<registryId>.dkr.ecr.<region>.<dnsSuffix>` — the host AWS ITSELF publishes the registry
/// account `registry_id` holds in `region` at, in the registry URL form
/// `AuthorizationData$proxyEndpoint` documents
/// (`https://aws_account_id.dkr.ecr.region.amazonaws.com`).
///
/// The same composition as [`announced_registry_host`] in the zone AWS serves that Region's
/// partition in ([`registry_dns_suffix`]), which is the spelling a caller that resolves
/// AWS's own hosts writes a reference in.
pub fn registry_host(registry_id: &str, region: &str) -> String {
    registry_host_in(registry_id, region, registry_dns_suffix(region))
}

/// The ONE composition of a registry host: the owning account's id, the two fixed labels,
/// the registry's Region, then the `zone` it is served in ([`REGISTRY_HOST_LABELS`]).
fn registry_host_in(registry_id: &str, region: &str, zone: &str) -> String {
    let [first, second] = REGISTRY_HOST_LABELS;
    format!("{registry_id}.{first}.{second}.{region}.{zone}")
}

/// What a registry host NAMES, read off an authority ([`registry_host_parts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryHostParts<'host> {
    /// The account whose registry the host names — its first label, a 12-digit account id.
    pub registry_id: &'host str,
    /// The Region that registry serves.
    pub region: &'host str,
    /// The zone the registry is served in: AWS's own partition suffix for a host AWS
    /// publishes, and the zone this emulator answers in for one it announced.
    pub zone: &'host str,
}

/// The account, the Region and the zone the registry host `authority` names, and `None` for
/// every authority of another shape.
///
/// The ONE reading of a registry host's layout, and the inverse of the one composition of it
/// ([`registry_host_in`]): what makes a host a registry's is the account label, the two
/// fixed labels ([`REGISTRY_HOST_LABELS`]) and the Region label — the zone that follows is
/// wherever the registry is SERVED, AWS's own partition suffix for a host AWS publishes and
/// this emulator's zone for a host it announced ([`announced_registry_host`]), so both
/// spellings of one registry read back as that registry. A pull asks which Region and
/// account the reference it carries addresses
/// (`super::registry_pull::parse_registry_reference`) and the data plane which Region the
/// client dialled ([`dialled_registry_region`]), so neither walks the labels itself.
///
/// A zone AWS publishes its OWN endpoints in, other than the Region's own partition suffix,
/// names no registry: AWS serves a Region's registries only under that suffix
/// ([`registry_dns_suffix`]), so `<account>.dkr.ecr.<a commercial Region>.amazonaws.com.cn`
/// and a deeper name in such a zone ([`names_an_aws_endpoint_zone`]) are hosts nothing
/// answers on. Every other zone is one this emulator may be announced in — the loopback zone,
/// or the name an operator serves it at — which no static list can enumerate.
///
/// Any userinfo and `:port` belong to the address rather than to the name and are removed
/// first; the fixed labels are compared without regard to case, as a host is.
///
/// What this does NOT answer is whether a URL names Amazon ECR as the PUBLIC registry a pull
/// through cache rule can mirror: that is the host AWS publishes, its own partition's suffix
/// included (`super::ecr_registry_host_parts`).
pub fn registry_host_parts(authority: &str) -> Option<RegistryHostParts<'_>> {
    let host = ::service_common::data_plane_addressing::host_only(authority);
    // The zone is the remainder: it carries labels of its own (`amazonaws.com` has two), so
    // only the labels ahead of it are split off.
    let mut labels = host.splitn(5, '.');
    // The account label is read by the tree's own reading of an account id
    // (`service_common::arn::is_account_id`, "for a member that carries an account id on its
    // own — … a registry or owner id"), so this reading and every other reader of one cannot
    // disagree.
    let registry_id = labels
        .next()
        .filter(|label| ::service_common::arn::is_account_id(label))?;
    for fixed in REGISTRY_HOST_LABELS {
        if !labels
            .next()
            .is_some_and(|label| label.eq_ignore_ascii_case(fixed))
        {
            return None;
        }
    }
    let region = labels.next().filter(|region| !region.is_empty())?;
    let zone = labels.next().filter(|zone| !zone.is_empty())?;
    let serves_the_region = zone.eq_ignore_ascii_case(registry_dns_suffix(region))
        || !names_an_aws_endpoint_zone(zone);
    serves_the_region.then_some(RegistryHostParts {
        registry_id,
        region,
        zone,
    })
}

/// Whether `zone` is one AWS publishes its OWN endpoints in: a DNS suffix a partition
/// declares ([`COMMERCIAL_DNS_SUFFIX`], [`PARTITION_DNS_SUFFIXES`]), or a deeper name inside
/// one (`service_common::data_plane_addressing::addresses_an_aws_endpoint`, which reads the
/// same suffixes with a label in front of them).
fn names_an_aws_endpoint_zone(zone: &str) -> bool {
    ::std::iter::once(COMMERCIAL_DNS_SUFFIX)
        .chain(PARTITION_DNS_SUFFIXES.iter().map(|(_, suffix)| *suffix))
        .any(|suffix| zone.eq_ignore_ascii_case(suffix))
        || ::service_common::data_plane_addressing::addresses_an_aws_endpoint(zone)
}

/// The DNS suffix the registries of `region`'s partition are served under: the `dnsSuffix`
/// that partition publishes in the AWS SDK partition metadata, which every regional
/// endpoint of the partition is built from.
///
/// A Region outside [`PARTITION_DNS_SUFFIXES`] is of the commercial partition, whose suffix
/// is the one the registry URL format above spells out.
pub fn registry_dns_suffix(region: &str) -> &'static str {
    PARTITION_DNS_SUFFIXES
        .iter()
        .find(|(prefix, _)| region.starts_with(prefix))
        .map_or(COMMERCIAL_DNS_SUFFIX, |(_, suffix)| *suffix)
}

/// The DNS suffix of the commercial partition, which the registry URL format
/// `AuthorizationData$proxyEndpoint` documents is written with.
pub const COMMERCIAL_DNS_SUFFIX: &str = "amazonaws.com";

/// The Region prefix of each partition whose endpoints are NOT under
/// [`COMMERCIAL_DNS_SUFFIX`], with the `dnsSuffix` that partition publishes in the AWS SDK
/// partition metadata (`endpoints.json`): `aws-cn` `amazonaws.com.cn`, `aws-eusc`
/// `amazonaws.eu`, `aws-iso` `c2s.ic.gov`, `aws-iso-b` `sc2s.sgov.gov`, `aws-iso-e`
/// `cloud.adc-e.uk`, `aws-iso-f` `csp.hci.ic.gov` — the same suffix set the core's own
/// reading of a partition host carries (`smithy_server_runtime::virtual_host`). GovCloud
/// (`us-gov-`) publishes `amazonaws.com`, so it needs no entry.
///
/// Every prefix ends in a hyphen, so `us-iso-` cannot swallow `us-isob-` and the order of
/// the entries carries no meaning.
pub const PARTITION_DNS_SUFFIXES: [(&str, &str); 6] = [
    ("cn-", "amazonaws.com.cn"),
    ("eusc-", "amazonaws.eu"),
    ("us-iso-", "c2s.ic.gov"),
    ("us-isob-", "sc2s.sgov.gov"),
    ("us-isof-", "csp.hci.ic.gov"),
    ("eu-isoe-", "cloud.adc-e.uk"),
];

/// `https://<registry host>` — the registry URL a `docker login` is pointed at, for the
/// registry `registry_host` names ([`announced_registry_host`]).
///
/// `AuthorizationData$proxyEndpoint` states the shape — "The Amazon ECR registry URL
/// format is `https://aws_account_id.dkr.ecr.region.amazonaws.com`" — a scheme followed by
/// the registry's own host, which here is that host in the zone this emulator answers the
/// caller in. The scheme is the one this emulator's endpoint carries.
pub fn registry_login_url(registry_host: &str) -> String {
    format!("https://{registry_host}")
}
