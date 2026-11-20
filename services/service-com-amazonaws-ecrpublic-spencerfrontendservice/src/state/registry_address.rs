// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS this public registry is reached at, and the two URIs it publishes on that
//! address.
//!
//! `Registry$registryUri` is "The URI of a public registry. The URI contains a universal
//! prefix and the registry alias", and `Repository$repositoryUri` is "The URI for the
//! repository. You can use this URI for container image `push` and `pull` operations". So
//! each is composed of two halves, and both halves are requirements:
//!
//! * the UNIVERSAL PREFIX — the authority every public registry of the partition is reached
//!   at, which is the fixed labels `public` and `ecr` ([`GALLERY_HOST_LABELS`]) followed by
//!   the DNS name the registry is served under. The leading labels are what makes an
//!   authority a public-registry authority at all: tooling reads them to tell a public
//!   registry URI from a private `<account>.dkr.ecr.<region>` one, and `docker login` is
//!   pointed at that authority;
//! * the registry ALIAS, the account's primary alias
//!   (`RegistryAlias$primaryRegistryAlias`, "displayed in both the repository URL and the
//!   image URI used in the docker pull commands"), as the single path segment of the
//!   registry's URI — followed by the repository's own name for a repository's URI.
//!
//! These URIs are also not labels a caller reads back: they are the addresses a container
//! client DIALS to move image bytes, so the requirement every other address this emulator
//! publishes carries holds for them too — THIS emulator has to answer there. AWS's own
//! gallery name ([`super::GALLERY_HOST`]) resolves to the real public registry rather than
//! to the machine this emulator runs on, so publishing it sends every push and pull out of
//! the emulator entirely: a client either reaches nothing or reaches an unrelated registry.
//!
//! Both requirements are met by composing the authority ([`gallery_authority`]) out of the
//! fixed labels and the DNS zone THIS emulator answers in, with the port it is served on —
//! `public.ecr.localhost:9297` for an emulator reached at a loopback address — so the
//! authority carries the registry identity AWS documents and still leads back here. The
//! zone is the tree's one reading of which zone this emulator may announce a name of its own
//! in (`service_common::data_plane_addressing::announced_zone_of_authority`): the reached
//! endpoint's own DNS name, or the loopback zone for an endpoint that names an ADDRESS
//! rather than a name (RFC 6761 reserves `localhost`, and every name under it, for the
//! loopback interface the core binds on).
//!
//! Which address the authority is read off is the endpoint the request reached
//! ([`announced_gallery_authority`]), so a reader is handed an address built on the endpoint
//! it is already talking to:
//!
//! * the endpoint the CALLER reached this emulator on, when that endpoint names no service
//!   (`service_common::data_plane_addressing::reached_emulator_endpoint`) — the emulator's
//!   own socket, a loopback name, or any host whose first label names none;
//! * the published gallery address itself, for a caller that came back to the authority this
//!   registry handed it ([`names_the_public_gallery`]) — whose first label is `public`, which
//!   that reading takes for a service name, so without this the address a client dialled
//!   would not be the one it is published next;
//! * otherwise the address this emulator is served at, because a caller that arrived on some
//!   service's endpoint host (an SDK pointed at `api.ecr-public.<region>.amazonaws.com`)
//!   would be handed a host that routes to a service, which no registry client can use.
//!
//! The composition AWS documents is kept throughout: the registry's URI is the authority
//! scoped by the registry's alias ([`registry_uri`]), and a repository's URI is the
//! registry's URI followed by the repository name ([`repository_uri`]), so a client splits
//! and extends what it was handed exactly as it does on AWS.
//!
//! Nothing a client reads such a URI FOR is lost: the registry alias is reported beside it
//! (`Registry$aliases`), and the owning account and the resource's identity are reported on
//! the very response that carries the URI (`Repository$registryId`,
//! `Repository$repositoryArn`) — those are LABELS, built from the stored record, and no
//! endpoint changes them.
//!
//! The URI is read per RESPONSE rather than stored with the repository, so the address a
//! reader is handed is the one that reader can reach, while the record itself is untouched
//! by who read it.

/// The two fixed labels every ECR Public registry authority LEADS with, as
/// `Registry$registryUri`'s "universal prefix" spells them (`public.ecr.<dns suffix>`, the
/// authority AWS publishes its public gallery at).
///
/// They are fixed text rather than anything about the account or the Region: the account
/// appears only in `Repository$registryId` and the registry's alias in the path, and a
/// public registry is global, so no Region appears in the authority at all. They are what a
/// client reads to tell a public registry URI apart from a private one, which is why the
/// authority this emulator publishes carries them ([`gallery_authority`]) and why a
/// reference written on one is read as naming the public gallery
/// ([`names_the_public_gallery`]).
pub const GALLERY_HOST_LABELS: [&str; 2] = ["public", "ecr"];

/// The authority every URI this registry publishes is built on, for the request being
/// served: the universal prefix above over the DNS zone this emulator answers in, carrying
/// the port it is served on (see the module docs).
///
/// A `host[:port]`, with no scheme — which is the form both modelled URIs carry.
pub fn announced_gallery_authority(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    let parts = ctx.parts();
    let reached =
        ::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts(parts)
            .or_else(|| addressed_gallery_authority(parts))
            .unwrap_or_else(|| {
                ::service_common::data_plane_addressing::emulator_endpoint_authority(ctx)
            });
    gallery_authority(&reached)
}

/// The published gallery address a request was ADDRESSED to, when that is what it names: the
/// address this registry handed a client, dialled back (see the module docs).
///
/// Only a value shaped like an address is taken
/// (`service_common::data_plane_addressing::is_host_shaped`), so nothing that is not one
/// reaches an authority this service publishes.
fn addressed_gallery_authority(parts: &::http::request::Parts) -> Option<String> {
    let addressed = ::service_common::data_plane_addressing::addressed_host(
        &parts.uri.to_string(),
        parts
            .headers
            .get(::http::header::HOST)
            .and_then(|value| value.to_str().ok()),
    )?;
    (::service_common::data_plane_addressing::is_host_shaped(&addressed)
        && names_the_public_gallery(&addressed))
    .then_some(addressed)
}

/// The public-registry authority to publish to a caller that reached this emulator at
/// `reached_authority`: the fixed [`GALLERY_HOST_LABELS`], the DNS zone this emulator may
/// announce a name of its own in, and the port that address is served on.
///
/// `public.ecr.localhost:9297` for an emulator reached at `127.0.0.1:9297` — the registry
/// authority AWS's own form carries, in a zone that leads back to this process (see the
/// module docs). An address already written as this gallery's keeps its zone rather than
/// gaining a second copy of the prefix, so the authority a client dialled is the one it is
/// published next.
pub fn gallery_authority(reached_authority: &str) -> String {
    use ::service_common::data_plane_addressing as addressing;

    let announced = addressing::announced_zone_of_authority(reached_authority);
    let zone = gallery_zone_of(&announced).unwrap_or(&announced);
    let [public, ecr] = GALLERY_HOST_LABELS;
    let host = format!("{public}.{ecr}.{zone}");
    match addressing::addressed_port(Some(reached_authority))
        .and_then(|port| u16::try_from(port).ok())
    {
        Some(port) => addressing::host_port_authority(&host, port),
        None => host,
    }
}

/// The zone a public-registry authority is served in — what follows the fixed
/// [`GALLERY_HOST_LABELS`] — and `None` for an authority that does not lead with them, which
/// names no public registry.
///
/// Any `:port` belongs to the address rather than to the name and is removed first
/// (`service_common::data_plane_addressing::host_only`); the labels are compared without
/// regard to case, as a host is.
pub fn gallery_zone_of(authority: &str) -> Option<&str> {
    let mut rest = ::service_common::data_plane_addressing::host_only(authority);
    for fixed in GALLERY_HOST_LABELS {
        let (label, tail) = rest.split_once('.')?;
        if !label.eq_ignore_ascii_case(fixed) {
            return None;
        }
        rest = tail;
    }
    (!rest.is_empty()).then_some(rest)
}

/// Whether `authority` names an ECR Public registry: an authority leading with the fixed
/// [`GALLERY_HOST_LABELS`] over a zone ([`gallery_zone_of`]).
///
/// Both the authority AWS publishes its gallery at ([`super::GALLERY_HOST`]) and the one
/// this emulator publishes ([`gallery_authority`]) are such a name, which is what lets a
/// container reference written on either be read as addressing this gallery
/// ([`super::gallery_pull::parse_gallery_reference`]) — the alias segment that follows then
/// says WHICH public registry it names, and only this registry's own alias is answered.
pub fn names_the_public_gallery(authority: &str) -> bool {
    gallery_zone_of(authority).is_some()
}

/// `<gallery authority>/<registry alias>` — `Registry$registryUri`, "a universal prefix and
/// the registry alias": the authority this registry is published at (`gallery_authority`,
/// read with [`announced_gallery_authority`]) scoped by the account's primary alias.
pub fn registry_uri(gallery_authority: &str, registry_alias: &str) -> String {
    format!(
        "{}/{registry_alias}",
        gallery_authority.trim_end_matches('/')
    )
}

/// `<registry URI>/<repository name>` — the URI a client pushes the repository
/// `repository_name` to and pulls it from.
///
/// The repository's own name is appended to the registry's URI exactly as AWS appends it,
/// including any `a/b/c` namespacing the modelled `RepositoryName` pattern admits, so a
/// client composes the same tag, digest or registry path on what it was handed.
pub fn repository_uri(registry_uri: &str, repository_name: &str) -> String {
    format!("{registry_uri}/{repository_name}")
}

/// Whether `host` — a `host[:port]` — names an address THIS emulator answers on, which is
/// the zone every URI this registry publishes is built in ([`gallery_authority`]).
///
/// This is the READING side of the publishing above, and it exists because the two have to
/// agree: a container reference a client composes on a `repositoryUri` it was handed carries
/// the address this registry published itself at, so the reading of such a reference
/// ([`super::gallery_pull::parse_gallery_reference`]) recognises that address as naming this
/// gallery. Without it a client that pushed to the URI the API gave it would be told the
/// registry knows nothing about the reference it was handed.
///
/// Every reading is the tree's own, and each is an address this emulator really answers on:
/// the loopback hosts a core listener admits ([`::emulator_server::is_loopback_host`]), the
/// host of the base this emulator reports while it is served on no port at all
/// ([`::smithy_server_runtime::UNSERVED_URL_BASE`], a name RFC 6761 reserves so nothing
/// resolves it), and the interface an operator bound this emulator on
/// ([`::emulator_server::bind_address_from_env`]) — the last only when that bind names ONE
/// address, since a wildcard bind names every interface and so says nothing about which
/// registry a reference addresses. That last point is why
/// `service_common::data_plane_addressing::host_names_the_bound_interface` is not the reading
/// here: it admits ANY host on a wildcard bind, which is right for deciding whether a request
/// reached a socket of this emulator and wrong for deciding whose registry a reference names.
///
/// Any `:port` is removed first ([`::service_common::data_plane_addressing::host_only`]): the
/// published URI carries the port the emulator is served on, and it is the host that says
/// whose registry a reference addresses.
pub fn names_this_emulator(host: &str) -> bool {
    let host = ::service_common::data_plane_addressing::host_only(host);
    if ::emulator_server::is_loopback_host(host) {
        return true;
    }
    let unserved = ::service_common::data_plane_addressing::url_authority(
        ::smithy_server_runtime::UNSERVED_URL_BASE,
    );
    if host.eq_ignore_ascii_case(unserved) {
        return true;
    }
    let Ok(bound) = ::emulator_server::bind_address_from_env() else {
        return false;
    };
    !bound.is_unspecified()
        && host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<::std::net::IpAddr>()
            .is_ok_and(|named| named == bound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{DEFAULT_REGISTRY_ALIAS, GALLERY_HOST};

    #[test]
    fn the_gallery_aws_serves_is_named_by_the_fixed_labels_and_its_own_zone() {
        // The host AWS publishes the gallery at is the fixed prefix over AWS's own zone, so
        // the one reading of such an authority recognises it.
        let [public, ecr] = GALLERY_HOST_LABELS;
        assert_eq!(
            gallery_zone_of(GALLERY_HOST),
            Some("aws"),
            "expected {GALLERY_HOST:?} to lead with {public}.{ecr}. and name its own zone"
        );
        assert!(
            names_the_public_gallery(GALLERY_HOST),
            "expected {GALLERY_HOST:?} to name a public registry"
        );
    }

    #[test]
    fn a_published_authority_is_the_universal_prefix_over_the_zone_this_emulator_answers_in() {
        // The rows span the endpoint forms a caller reaches this emulator by: a socket, a
        // loopback name with a port, a bracketed IPv6 literal, a bare host name, the base
        // reported while the emulator is served on no port at all, and the published gallery
        // address itself — which keeps its zone rather than gaining a second prefix.
        let cases: [(&str, &str); 7] = [
            ("127.0.0.1:9297", "public.ecr.localhost:9297"),
            ("127.0.0.1", "public.ecr.localhost"),
            ("localhost:39355", "public.ecr.localhost:39355"),
            ("[::1]:8443", "public.ecr.localhost:8443"),
            ("emulator.invalid", "public.ecr.emulator.invalid"),
            ("public.ecr.localhost:9297", "public.ecr.localhost:9297"),
            (
                "api.ecr-public.us-east-1.amazonaws.com:443",
                "public.ecr.localhost:443",
            ),
        ];
        for (reached, expected) in cases {
            let published = gallery_authority(reached);
            assert_eq!(
                published, expected,
                "case {reached:?}: expected the published authority {expected:?}, got \
                 {published:?}"
            );
            let zone = gallery_zone_of(&published).unwrap_or_else(|| {
                panic!("case {reached:?}: expected {published:?} to name a public registry")
            });
            // The zone is one this emulator answers in, never a zone AWS serves its own
            // gallery or its endpoints in.
            assert!(
                names_this_emulator(zone),
                "case {reached:?}: expected a zone this emulator answers in, got {zone:?}"
            );
        }
    }

    #[test]
    fn an_authority_that_names_no_public_registry_is_told_from_one_that_does() {
        // The rows span the authorities a container reference can carry: the gallery AWS
        // serves, the one this emulator publishes, another registry altogether, an Amazon ECR
        // private registry host, and the prefix written without a zone behind it.
        let cases: [(&str, bool); 8] = [
            (GALLERY_HOST, true),
            ("PUBLIC.ECR.AWS", true),
            ("public.ecr.localhost:9297", true),
            ("public.ecr.emulator.invalid", true),
            ("public.ecr.", false),
            ("public.ecr", false),
            ("000000000000.dkr.ecr.us-east-1.amazonaws.com", false),
            ("quay.io", false),
        ];
        for (authority, expected) in cases {
            let named = names_the_public_gallery(authority);
            assert_eq!(
                named, expected,
                "case {authority:?}: expected names_the_public_gallery to be {expected}, got \
                 {named}"
            );
        }
    }

    #[test]
    fn an_address_this_emulator_answers_on_is_told_from_one_it_does_not() {
        // The rows span the hosts a published URI's zone can carry — the loopback forms a core
        // listener admits, with and without a port, and the base the emulator reports while
        // it is served on no port at all — against the registry hosts a reference names
        // instead: the gallery AWS serves, another registry altogether, and an Amazon ECR
        // private registry host.
        let cases: [(&str, bool); 10] = [
            ("127.0.0.1:9297", true),
            ("127.0.0.1", true),
            ("127.3.2.1:8443", true),
            ("localhost:39355", true),
            ("[::1]:9297", true),
            ("emulator.invalid", true),
            (GALLERY_HOST, false),
            ("quay.io", false),
            ("000000000000.dkr.ecr.us-east-1.amazonaws.com", false),
            ("10.1.2.3:5000", false),
        ];
        for (host, expected) in cases {
            let answered = names_this_emulator(host);
            assert_eq!(
                answered, expected,
                "case {host:?}: expected names_this_emulator to be {expected}, got {answered}"
            );
        }
    }

    #[test]
    fn a_repository_uri_extends_the_registry_uri_it_is_held_in() {
        // The rows span what the composition generalises over: the authority this registry is
        // published at for each endpoint form a caller reaches this emulator by (a socket, a
        // loopback name with a port, a bracketed IPv6 literal, the unserved base, and one
        // written with a trailing slash) crossed with the repository-name forms the modelled
        // `RepositoryName` pattern admits.
        let authorities = [
            gallery_authority("127.0.0.1:9297"),
            gallery_authority("localhost:39355"),
            gallery_authority("[::1]:9297"),
            gallery_authority("emulator.invalid"),
            format!("{}/", gallery_authority("127.0.0.1:9297")),
        ];
        let names = ["ab", "nginx", "team.app", "project-a/nginx-web-app"];
        for authority in &authorities {
            let registry = registry_uri(authority, DEFAULT_REGISTRY_ALIAS);
            let expected_registry = format!(
                "{}/{DEFAULT_REGISTRY_ALIAS}",
                authority.trim_end_matches('/')
            );
            assert_eq!(
                registry, expected_registry,
                "case {authority:?}: expected the registry URI to be the authority scoped by \
                 the alias, got {registry:?}"
            );
            for name in names {
                let uri = repository_uri(&registry, name);
                let case = format!("{authority} {name}");
                assert_eq!(
                    uri,
                    format!("{registry}/{name}"),
                    "case {case:?}: expected the repository URI under the registry URI, \
                     got {uri:?}"
                );
                // The authority half is the address the client dials, and the rest is the
                // path it extends: a public-registry authority, and never the real gallery
                // host, which this emulator does not answer on.
                let (dialled, path) = uri
                    .split_once('/')
                    .unwrap_or_else(|| panic!("case {case:?}: {uri:?} names no path"));
                assert_eq!(
                    dialled,
                    authority.trim_end_matches('/'),
                    "case {case:?}: expected the published authority as the dialled one, \
                     got {dialled:?}"
                );
                assert!(
                    names_the_public_gallery(dialled),
                    "case {case:?}: expected a public-registry authority, got {dialled:?}"
                );
                assert_eq!(
                    path,
                    format!("{DEFAULT_REGISTRY_ALIAS}/{name}"),
                    "case {case:?}: expected the alias-scoped repository path, got {path:?}"
                );
                assert_ne!(
                    dialled, GALLERY_HOST,
                    "case {case:?}: a published URI must not name the real gallery host"
                );
            }
        }
    }
}
