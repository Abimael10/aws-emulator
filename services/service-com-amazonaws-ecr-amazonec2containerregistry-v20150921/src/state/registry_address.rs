// SPDX-License-Identifier: CC0-1.0

//! The registry ADDRESS this service REPORTS for a repository — the one a container client
//! writes into an image reference — and the reading of a registry host a client wrote a
//! reference in.
//!
//! # What a repository's own URI has to be
//!
//! `Repository$repositoryUri` is "The URI for the repository. You can use this URI for
//! container image `push` and `pull` operations", so it is read two ways at once: a container
//! client DIALS it to move image bytes, and a client READS it for the registry that holds the
//! repository — the owning account's id and the Region are the leading labels of an Amazon
//! ECR registry host, which is what lets a reader recover the account and the Region from the
//! URI it was told to push and pull with and build an `ecr:` ARN, a `docker login` target or
//! an image reference of a compute service from it.
//!
//! So the authority it carries is the registry host layout Amazon ECR publishes — the owning
//! account's id, the two fixed labels ([`REGISTRY_HOST_LABELS`]) and the registry's Region,
//! both read off the stored record ([`registry_host_in_zone`]) — over the DNS ZONE this
//! emulator may announce a name of its own in and the port that caller reaches this emulator
//! on ([`announced_registry_authority`]), with the repository's name as the path
//! ([`repository_uri`]): a client splits and extends what it was handed exactly as it does on
//! AWS, and the address it dials still leads back to this process.
//!
//! The zone AWS serves its own registries under ([`registry_dns_suffix`], giving
//! [`registry_host`]) is NOT a zone this emulator may announce in: a name under it resolves
//! to the real Amazon ECR rather than to the machine this emulator runs on, so publishing one
//! sends every push and pull out of the emulated environment entirely, carrying the
//! credentials the client holds. The zone of the address this emulator is REACHED at is such
//! a zone, which is why the workspace's one reading of that question
//! (`service_common::data_plane_addressing::announced_zone_of_authority`) picks it — the
//! loopback zone RFC 6761 reserves for the interface the core binds on, where that address is
//! an address rather than a name.
//!
//! The URI is composed per RESPONSE rather than stored on the record, so the address a reader
//! is handed is one that reader can reach while the record itself is untouched by who read it;
//! which registry HOLDS the repository is reported beside it, by `Repository$registryId` and
//! `Repository$repositoryArn`, and those are LABELS off the stored record that no reader's
//! endpoint moves.
//!
//! A reference a client brings IN is the other side of this. It may be written on the
//! authority this registry published, or by hand on the host AWS publishes, and the two differ
//! only in the zone they are served in, so the ONE reading of a registry host
//! ([`registry_host_parts`]) reads both back as naming this registry — the account, the fixed
//! labels and the Region are the same labels in either — and this registry's `/v2` data plane
//! claims a request whatever host it arrived on (`crate::service::registry_v2`). Either way
//! the client reaches the very repository the URI names.
//!
//! # What a `docker login` is pointed at
//!
//! `AuthorizationData$proxyEndpoint` ("The registry URL to use for this authorization token
//! in a `docker login` command") is not a reference a reader parses: it is the URL a client
//! DIALS as given, with the credential the token carries, so it is built on the endpoint
//! this emulator answers THAT caller's registry requests at ([`announced_registry_endpoint`],
//! the tree's one reading of that question) behind a scheme ([`registry_login_url`]) — the
//! one address a client on this machine can open a connection to without resolving a name at
//! all. A repository's own URI is the other case: a reader PARSES it for the registry that
//! holds the repository, so it carries the registry host layout above over a zone this
//! emulator answers in, and the credential this URL was issued with is honoured there too.
//!
//! An Amazon ECR registry is one account's in ONE Region, which is why a repository is keyed
//! by Region (`super::repository_key`) and why its URI names that Region: a `/v2` request
//! names the repository in the path and the registry through the host it was addressed to
//! ([`dialled_registry_region`]) or, for an endpoint naming no registry, the credential it
//! presents (`super::registry_v2::RegistryCredential::region`), which is the registry the
//! token was issued for.
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

/// The endpoint this emulator answers THIS caller's registry requests at: the address the
/// registry URL a `docker login` of that caller is pointed at is built on
/// ([`registry_login_url`]), and the address the registry authority a repository's own URI
/// carries is anchored on ([`announced_registry_authority`]) — its zone and its port.
///
/// Two endpoints can satisfy that:
///
/// - the endpoint the CALLER reached this emulator on, whenever this registry answers a
///   `/v2` request addressed there ([`answers_registry_requests_at`]) — the emulator's own
///   socket, a loopback name, a registry host itself, and this service's own endpoint host,
///   because a request routed to THIS service by its host is served by this service's
///   registry plane (`crate::service`'s `override_routing_error`). Such a caller is reported
///   the address it is already talking to, which is the one address it is known to reach;
/// - otherwise the address this emulator is served at: a caller that came in on ANOTHER
///   service's endpoint host would be handed an address a registry client cannot use,
///   because that host routes to that service and no registry request addressed to it
///   reaches this plane — and so would a caller that came in on a host inside a zone AWS
///   publishes its OWN endpoints in (`addresses_an_aws_endpoint`), which resolves nowhere
///   near this process. Taking the served address WHOLE, host and port together, is what
///   keeps the reported address dialable.
pub fn announced_registry_endpoint(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> String {
    let parts = ctx.parts();
    ::service_common::data_plane_addressing::addressed_host(
        &parts.uri.to_string(),
        parts
            .headers
            .get(::http::header::HOST)
            .and_then(|value| value.to_str().ok()),
    )
    .filter(|reached| answers_registry_requests_at(reached, ctx.identity().signing_name()))
    .unwrap_or_else(|| {
        ::service_common::data_plane_addressing::emulator_endpoint_authority(ctx)
    })
}

/// Whether a `/v2` request addressed to `reached` — the `host[:port]` a caller stated — is
/// one THIS registry answers, which is what makes that authority reportable as the address a
/// `docker login` is pointed at ([`announced_registry_endpoint`]).
///
/// Three readings, each the tree's own:
///
/// - it has to be an ADDRESS at all
///   (`service_common::data_plane_addressing::is_host_shaped`), so nothing a caller wrote
///   that is not a `host[:port]` reaches a URL this service announces;
/// - it must not be a host AWS publishes its OWN endpoints at
///   (`addresses_an_aws_endpoint`): such a name resolves nowhere near this process or, worse,
///   at genuine AWS infrastructure, which sends the next client out of the emulated
///   environment carrying the credential it is holding;
/// - it must name no OTHER service than `this_service`
///   (`service_common::data_plane_addressing::service_named_by_host`, the reading written
///   for exactly this question — "for a plane that has to tell its OWN service's endpoint
///   from another service's"): a host the front door resolves to another service routes
///   there, so no registry request addressed to it reaches this plane, while a host that
///   resolves to THIS service reaches it (`crate::service`), as does one that names no
///   service at all — and so does a host that names THIS REGISTRY
///   (`super::registry_pull::host_region`), which is the authority this service itself
///   announced for a repository: its first label is the owning account's id, which the
///   reading above cannot tell from a service's label, and a request addressed there is one
///   this plane claims. Reading it back here is also what keeps the announced authority
///   STABLE: a caller that dialled it is reported it again rather than a second host composed
///   on top of the first.
fn answers_registry_requests_at(reached: &str, this_service: &str) -> bool {
    use ::service_common::data_plane_addressing as addressing;

    if !addressing::is_host_shaped(reached) || addressing::addresses_an_aws_endpoint(reached) {
        return false;
    }
    super::registry_pull::host_region(reached).is_some()
        || addressing::service_named_by_host(reached)
            .is_none_or(|named| named == this_service)
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

/// `<authority>/<name>` — `Repository$repositoryUri`, the URI a client pushes the repository
/// `name` to and pulls it from: the registry authority this service publishes to that caller
/// (read with [`announced_registry_authority`]) followed by the repository's own name as the
/// path.
///
/// The path after the first `/` is exactly the repository name, so a client composes a tag, a
/// digest or a `/v2` path on what it was handed and the reference reads back as the
/// repository it names (`super::registry_pull::parse_reference`, which reads the registry
/// host layout the authority carries whichever zone it is served in).
pub fn repository_uri(announced_authority: &str, name: &str) -> String {
    format!("{}/{name}", announced_authority.trim_end_matches('/'))
}

/// The AUTHORITY this registry PUBLISHES for the registry the account `registry_id` owns in
/// `region`, to a caller this emulator answers at `announced_endpoint` (read with
/// [`announced_registry_endpoint`]): an Amazon ECR registry host over the zone this emulator
/// may announce a name of its own in, carrying the port that caller reaches this emulator on.
///
/// Both halves of what `Repository$repositoryUri` is for hold at once (see the module docs):
///
/// - the LAYOUT is the one Amazon ECR publishes for a registry host
///   ([`registry_host_in_zone`]) — the owning account's id, the two fixed labels and the
///   Region, every one of them read off the stored record rather than off the request — so a
///   reader recovers the account and the Region from the URI and can build an `ecr:` ARN, a
///   login target or a compute service's image reference out of it;
/// - the ZONE is the one this emulator may announce in
///   (`service_common::data_plane_addressing::announced_zone_of_authority`, the workspace's
///   one reading of that question), read off the endpoint the caller reached this emulator
///   at, and the PORT is that endpoint's own, so the address a client dials leads back to
///   this process. Where that endpoint is already a registry authority this service
///   announced, its own zone is kept, so the value a reader is handed again is the value it
///   dialled.
///
/// `region` is the Region of the registry holding the repository, which ends up in a host
/// here, so it is taken only as a Region LABEL (`service_common::arn::region_label`, this
/// tree's one statement of that grammar) and anything else falls back to
/// [`DEFAULT_REGION`](super::DEFAULT_REGION): a value carrying a host or path delimiter would
/// name an authority the caller chose.
pub fn announced_registry_authority(
    announced_endpoint: &str,
    registry_id: &str,
    region: &str,
) -> String {
    let region = ::service_common::arn::region_label(region).unwrap_or(super::DEFAULT_REGION);
    let host = registry_host_in_zone(registry_id, region, &announced_zone(announced_endpoint));
    match ::service_common::data_plane_addressing::addressed_port(Some(announced_endpoint)) {
        Some(port) => format!("{host}:{port}"),
        None => host,
    }
}

/// The DNS zone the registry authority announced to a caller reached at `announced_endpoint`
/// is served in ([`announced_registry_authority`]).
///
/// The zone of that endpoint, by the workspace's one reading of which zone this emulator may
/// announce a name of its own in — except that an endpoint which is ITSELF a registry
/// authority (the one this service announced, which a client then dialled) is read for the
/// zone it already carries ([`registry_host_parts`]), so the announced authority is the same
/// value however many times a client is handed it. A registry host served under a zone AWS
/// publishes its own endpoints in ([`names_an_aws_endpoint_zone`]) is never such an endpoint:
/// nothing here answers for a name AWS serves.
fn announced_zone(announced_endpoint: &str) -> String {
    registry_host_parts(announced_endpoint)
        .map(|parts| parts.zone)
        .filter(|zone| !names_an_aws_endpoint_zone(zone))
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| {
            ::service_common::data_plane_addressing::announced_zone_of_authority(
                announced_endpoint,
            )
        })
}

/// The host of the Amazon ECR registry `registry_id` owns in `region`, in the spelling AWS
/// PUBLISHES: the owning account's id, the two fixed labels ([`REGISTRY_HOST_LABELS`]), the
/// Region, and the DNS suffix that Region's partition is served under
/// ([`registry_dns_suffix`]).
///
/// The registry URL format the model states, `aws_account_id.dkr.ecr.region.amazonaws.com`
/// (`AuthorizationData$proxyEndpoint`), with the partition's own suffix in place of the
/// commercial one — so a registry of any partition's Region is named the way that partition
/// publishes it. It is the inverse of the one reading of such a host
/// ([`registry_host_parts`]), so a reference written in that spelling reads back as the
/// registry it names.
///
/// It is the host every repository of that registry is addressed under ON AWS, so it is the
/// spelling a client's own reference may be written in. What this registry PUBLISHES for a
/// repository is the same layout over a zone this emulator ANSWERS in
/// ([`announced_registry_authority`]), because nothing here answers for a name AWS serves.
pub fn registry_host(registry_id: &str, region: &str) -> String {
    registry_host_in_zone(registry_id, region, registry_dns_suffix(region))
}

/// `<registry id>.dkr.ecr.<region>.<zone>` — an Amazon ECR registry host in `zone`: the
/// layout AWS publishes for a registry ([`registry_host`] is its case for the zone AWS serves
/// a Region's registries in), with the zone left open because the registry this emulator
/// serves is announced in a zone this emulator answers in
/// ([`announced_registry_authority`]).
///
/// The ONE composer of such a host, so the authority this service announces and the authority
/// AWS publishes cannot drift in their labels or their order, and the one reading of such a
/// host ([`registry_host_parts`]) reads either back.
pub fn registry_host_in_zone(registry_id: &str, region: &str, zone: &str) -> String {
    let [dkr, ecr] = REGISTRY_HOST_LABELS;
    format!("{registry_id}.{dkr}.{ecr}.{region}.{zone}")
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
/// on, because that URL is DIALLED as it is handed over, with the credential the token
/// carries, and the endpoint the caller already reached is the one address this service
/// knows it can reach. A repository's own URI is published on that same endpoint, with the
/// repository's name as its path and no scheme, which is the form that member carries
/// ([`repository_uri`]).
pub fn registry_login_url(announced_endpoint: &str) -> String {
    format!("https://{announced_endpoint}")
}
