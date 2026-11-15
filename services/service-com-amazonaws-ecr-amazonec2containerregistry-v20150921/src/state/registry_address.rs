// SPDX-License-Identifier: CC0-1.0

//! The registry URI a repository is NAMED by, and the endpoint a registry address this
//! service ANNOUNCES for a client to DIAL.
//!
//! `Repository$repositoryUri` ("The URI for the repository. You can use this URI for
//! container image `push` and `pull` operations") names the registry that HOLDS the
//! repository, in the registry URL form this model itself writes down
//! (`AuthorizationData$proxyEndpoint`: "The Amazon ECR registry URL format is
//! `https://aws_account_id.dkr.ecr.region.amazonaws.com`"): the account id of the registry,
//! the two fixed labels, the Region that registry serves, and the partition's DNS suffix,
//! followed by `/<repositoryName>`. The account and the Region are what the value is FOR —
//! a client derives from them the registry it authenticates to, the `image` of an ECS task
//! definition, and whether an image reference belongs to its own account and Region — so
//! the URI is composed from the repository's own `registryId` and Region
//! ([`repository_uri`]), never from the socket this emulator happens to listen on, out of
//! which neither can be recovered. Nothing is lost by naming it: this registry's `/v2`
//! data plane claims a request whatever host it arrived on (`crate::service::registry_v2`),
//! so a client whose resolver points that host at this emulator pulls the repository the
//! URI names, and the Region in the host is the one that plane reads the registry from
//! ([`dialled_registry_region`]).
//!
//! `AuthorizationData$proxyEndpoint` ("The registry URL to use for this authorization
//! token in a `docker login` command") is not a label a caller reads back: it is the one
//! address a container client is handed to DIAL, so the requirement on it is that THIS
//! emulator answers the registry API there. AWS's own
//! `<accountId>.dkr.ecr.<region>.amazonaws.com` host does not resolve to this emulator on
//! the machine it runs on, so a login address naming it sends the client out of the
//! emulator entirely.
//!
//! Two endpoints can satisfy that, and [`announced_registry_endpoint`] picks between them
//! with the tree's one reading of the question
//! (`service_common::data_plane_addressing::reached_emulator_endpoint`):
//!
//! - the endpoint the CALLER reached this emulator on, when that endpoint names no
//!   service — the emulator's own socket, a loopback name, and the registry host itself,
//!   whose first label is an account id. `crate::service`'s registry data plane claims a
//!   `/v2` request exactly when its endpoint names no service, so such a caller is
//!   handed the address it is already talking to and reaches the registry there;
//! - otherwise the address this emulator is served at, because a caller that came in on
//!   some service's endpoint host (an SDK pointed at `api.ecr.<region>.amazonaws.com`) is
//!   handed an address a registry client cannot use: that host routes to a service, so no
//!   registry request addressed to it reaches this plane.
//!
//! The Region a call is signed for names the repository's ARN and its URI, which are
//! labels rather than addresses; the one endpoint a client dials serves every Region here.

/// The endpoint every registry address this service announces is built on: the endpoint
/// the caller reached this emulator on when a registry client can use it, and the address
/// this emulator is served at otherwise (see the module docs).
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
    .unwrap_or_else(|| {
        ::service_common::data_plane_addressing::emulator_endpoint_authority(ctx)
    })
}

/// The Region of the registry a request to one of this registry's own DATA-PLANE endpoints
/// DIALLED: the Region named by the `<accountId>.dkr.ecr.<region>.amazonaws.com` endpoint it
/// was addressed to, read through the one reading of that host this service states
/// (`super::registry_pull::host_region`), and `None` for an endpoint that names no Region —
/// this emulator's own socket, which serves every Region's registry.
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
    let authority = authority.to_ascii_lowercase();
    let host = authority.split(':').next().unwrap_or_default();
    super::registry_pull::host_region(host)
}

/// `<registryId>.dkr.ecr.<region>.<dnsSuffix>/<name>` — the URI that names the repository
/// `name` of the registry account `registry_id` holds in `region` (see the module docs).
///
/// The repository's own name is appended to the registry host exactly as AWS appends it, so
/// a client composes the same tag, digest or registry v2 path on what it was handed.
pub fn repository_uri(registry_id: &str, region: &str, name: &str) -> String {
    format!("{}/{name}", registry_host(registry_id, region))
}

/// `<registryId>.dkr.ecr.<region>.<dnsSuffix>` — the host of the registry account
/// `registry_id` holds in `region`, in the registry URL form
/// `AuthorizationData$proxyEndpoint` documents
/// (`https://aws_account_id.dkr.ecr.region.amazonaws.com`).
///
/// It is the one composition of that host this service makes, and the inverse of the one
/// reading of it (`super::ecr_registry_host_parts`), so a URI this registry announces
/// parses back as a reference to the very repository it names.
pub fn registry_host(registry_id: &str, region: &str) -> String {
    format!(
        "{registry_id}.dkr.ecr.{region}.{}",
        registry_dns_suffix(region)
    )
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

/// `https://<registry endpoint>` — the registry URL a `docker login` is pointed at,
/// announced at `registry_endpoint` ([`announced_registry_endpoint`]).
///
/// `AuthorizationData$proxyEndpoint` states the shape — "The Amazon ECR registry URL
/// format is `https://aws_account_id.dkr.ecr.region.amazonaws.com`" — a scheme followed by
/// the registry's endpoint, which here is the endpoint this emulator serves that registry
/// at. The scheme is the one this emulator's endpoint carries.
pub fn registry_login_url(registry_endpoint: &str) -> String {
    format!("https://{registry_endpoint}")
}
