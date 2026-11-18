// SPDX-License-Identifier: CC0-1.0

//! The registry ADDRESS this service REPORTS for a repository — the one a container client
//! writes into an image reference — and the reading of a registry host a client wrote a
//! reference in.
//!
//! # What the reported address has to be
//!
//! `Repository$repositoryUri` ("The URI for the repository. You can use this URI for
//! container image `push` and `pull` operations") is the repository's address IN ITS OWN
//! REGISTRY, and an Amazon ECR registry is named by the account that owns it and the Region
//! it serves: the registry URL format the model spells out is
//! `aws_account_id.dkr.ecr.region.amazonaws.com` (`AuthorizationData$proxyEndpoint`), with
//! the partition's own DNS suffix in place of `amazonaws.com` outside the commercial
//! partition. So the URI a repository is reported at is composed from the record ITSELF —
//! the owning account, the Region that keys the repository and the repository's own name
//! ([`repository_uri`]) — and is therefore the same value for every reader and a DIFFERENT
//! value in every Region, which is what lets a client keep two Regions' repositories of one
//! name apart and recover the account and the Region from a reference written on the URI
//! (an ECS task definition's image, a Lambda `ImageUri`, a `docker push` target).
//!
//! `AuthorizationData$proxyEndpoint` ("The registry URL to use for this authorization token
//! in a `docker login` command") is a different kind of value: it is the URL a client DIALS
//! with the credential it was just issued, so it names the endpoint the caller is already
//! talking to ([`announced_registry_endpoint`], the tree's one reading of that question)
//! behind a scheme ([`registry_login_url`]) — the one address this service knows that client
//! can reach, which is also where this registry's `/v2` data plane answers.
//!
//! An Amazon ECR registry is one account's in ONE Region, which is why a repository is keyed
//! by Region (`super::repository_key`) even though one endpoint answers for every Region: a
//! `/v2` request names the repository in the path and the registry through the host it was
//! addressed to ([`dialled_registry_region`]) or the credential it presents
//! (`super::registry_v2::RegistryCredential::region`), which is the registry the token was
//! issued for.
//!
//! # Reading a registry host a client wrote
//!
//! A reference a client brings IN is another matter: it is written in the registry URL form
//! AWS publishes, so a pull through cache rule's upstream URL, an in-process pull's image
//! reference and the host a `/v2` request was addressed to are read back through the one
//! reading of that layout ([`registry_host_parts`]) — the owning account's id, the two fixed
//! labels, the Region, then the zone the registry is served in. That reading is what says
//! which Region's registry a client dialled ([`dialled_registry_region`]), and this
//! registry's `/v2` data plane claims a request whatever host it arrived on
//! (`crate::service::registry_v2`), so both a reference written on AWS's spelling and one
//! written on the address this service reported reach the very repository they name.

/// The endpoint this emulator answers THIS caller at, which is the address the registry URL
/// a `docker login` of that caller is pointed at is built on ([`registry_login_url`]).
///
/// Two endpoints can satisfy that, and the tree's one reading of the question picks between
/// them (`service_common::data_plane_addressing::reached_emulator_endpoint`):
///
/// - the endpoint the CALLER reached this emulator on, when that endpoint names no
///   service — the emulator's own socket, a loopback name, and a registry host itself,
///   whose first label is an account id. `crate::service`'s registry data plane claims a
///   `/v2` request exactly when its endpoint names no service, so such a caller is reported
///   the address it is already talking to and reaches the registry there;
/// - otherwise the address this emulator is served at: a caller that came in on some
///   service's endpoint host (an SDK pointed at `api.ecr.<region>.amazonaws.com`) would be
///   handed an address a registry client cannot use, because that host routes to a service
///   and no registry request addressed to it reaches this plane — and so would a caller that
///   came in on a host inside a zone AWS publishes its OWN endpoints in
///   (`addresses_an_aws_endpoint`), which resolves nowhere near this process. Taking the
///   served address WHOLE, host and port together, is what keeps the reported address
///   dialable.
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
/// from: the host of the registry that holds it ([`registry_host`]) followed by the
/// repository's own name as the path.
///
/// Composed from the repository's own record — the account that owns the registry, the
/// Region that registry serves and the repository name — so every reader is reported the
/// same value and each Region's registry reports its own. The path after the first `/` is
/// exactly the repository name, so a client composes a tag, a digest or a `/v2` path on what
/// it was handed and the reference reads back as the repository it names
/// ([`registry_host_parts`], `super::registry_pull::parse_reference`).
pub fn repository_uri(registry_id: &str, region: &str, name: &str) -> String {
    format!("{}/{name}", registry_host(registry_id, region))
}

/// The host of the Amazon ECR registry `registry_id` owns in `region`: the owning account's
/// id, the two fixed labels ([`REGISTRY_HOST_LABELS`]), the Region, and the DNS suffix that
/// Region's partition is served under ([`registry_dns_suffix`]).
///
/// The registry URL format the model states, `aws_account_id.dkr.ecr.region.amazonaws.com`
/// (`AuthorizationData$proxyEndpoint`), with the partition's own suffix in place of the
/// commercial one — so a registry of any partition's Region is named the way that partition
/// publishes it. It is the inverse of the one reading of such a host
/// ([`registry_host_parts`]), so an address this registry reports reads back as the registry
/// it names.
pub fn registry_host(registry_id: &str, region: &str) -> String {
    let [dkr, ecr] = REGISTRY_HOST_LABELS;
    format!(
        "{registry_id}.{dkr}.{ecr}.{region}.{}",
        registry_dns_suffix(region)
    )
}

/// The two fixed labels every Amazon ECR registry host carries between the owning account's
/// id and the registry's Region, as the registry URL format
/// `AuthorizationData$proxyEndpoint` documents spells them
/// (`https://aws_account_id.dkr.ecr.region.amazonaws.com`).
///
/// The one reading of such a host ([`registry_host_parts`]) is written on them, so a
/// reference a client wrote in AWS's spelling parses back as a reference to the registry it
/// names.
pub const REGISTRY_HOST_LABELS: [&str; 2] = ["dkr", "ecr"];

/// What a registry host NAMES, read off an authority ([`registry_host_parts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryHostParts<'host> {
    /// The account whose registry the host names — its first label, a 12-digit account id.
    pub registry_id: &'host str,
    /// The Region that registry serves.
    pub region: &'host str,
    /// The zone the registry is served in: AWS's own partition suffix for the host AWS
    /// publishes, and a zone of its own for a host this emulator is reached at.
    pub zone: &'host str,
}

/// The account, the Region and the zone the registry host `authority` names, and `None` for
/// every authority of another shape.
///
/// The ONE reading of a registry host's layout: what makes a host a registry's is the account
/// label, the two fixed labels ([`REGISTRY_HOST_LABELS`]) and the Region label — the zone
/// that follows is wherever the registry is SERVED, AWS's own partition suffix for the host
/// AWS publishes and a zone of its own for a host an operator points a client at, so every
/// spelling of one registry reads back as that registry. A pull asks which Region and
/// account the reference it carries addresses
/// (`super::registry_pull::parse_registry_reference`) and the data plane which Region the
/// client dialled ([`dialled_registry_region`]), so neither walks the labels itself.
///
/// A zone AWS publishes its OWN endpoints in, other than the Region's own partition suffix,
/// names no registry: AWS serves a Region's registries only under that suffix
/// ([`registry_dns_suffix`]), so `<account>.dkr.ecr.<a commercial Region>.amazonaws.com.cn`
/// and a deeper name in such a zone ([`names_an_aws_endpoint_zone`]) are hosts nothing
/// answers on. Every other zone is one a client may have been pointed at — the loopback zone,
/// or the name an operator serves this emulator at — which no static list can enumerate.
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

/// `https://<endpoint>` — the registry URL a `docker login` is pointed at: the endpoint this
/// emulator answers the caller at ([`announced_registry_endpoint`]), behind a scheme.
///
/// `AuthorizationData$proxyEndpoint` states the shape — "The Amazon ECR registry URL
/// format is `https://aws_account_id.dkr.ecr.region.amazonaws.com`" — a scheme followed by
/// the registry's address, which here is the address this emulator answers the registry API
/// on, because that URL is DIALLED with the credential the token carries and the endpoint
/// the caller already reached is the one address this service knows it can reach. A
/// repository's own URI is a different value: it names the registry that HOLDS the
/// repository ([`repository_uri`]), composed from the stored record.
pub fn registry_login_url(announced_endpoint: &str) -> String {
    format!("https://{announced_endpoint}")
}
