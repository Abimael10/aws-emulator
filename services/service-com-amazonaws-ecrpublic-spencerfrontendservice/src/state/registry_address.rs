// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS this public registry is reached at, and the two URIs it publishes on that
//! address.
//!
//! `Registry$registryUri` is "The URI of a public registry. The URI contains a universal
//! prefix and the registry alias", and `Repository$repositoryUri` is "The URI for the
//! repository. You can use this URI for container image `push` and `pull` operations". So
//! neither is a label a caller reads back: both are the addresses a container client DIALS
//! to move image bytes, and the requirement every other address this emulator publishes
//! carries holds for them too — THIS emulator has to answer there.
//!
//! So each is published on the endpoint this emulator answers THAT caller's requests at
//! ([`announced_gallery_authority`]), keeping the composition AWS documents on it: the
//! registry's URI is that endpoint scoped by the registry's alias ([`registry_uri`]) — the
//! "universal prefix and the registry alias" — and a repository's URI is the registry's URI
//! followed by the repository name ([`repository_uri`]), so a client splits and extends what
//! it was handed exactly as it does on AWS.
//!
//! Neither the name AWS serves its own gallery at ([`super::GALLERY_HOST`]) nor any name
//! composed on the `public`.`ecr` prefix ([`GALLERY_HOST_LABELS`]) is such an endpoint:
//! AWS's own resolves to the real public registry rather than to the machine this emulator
//! runs on, and a name this emulator invented under that prefix resolves nowhere at all —
//! no listener of this emulator is bound to one, and this service serves no registry route
//! under it. Publishing either sends every push and pull out of the emulator entirely: a
//! client either reaches nothing or reaches an unrelated registry.
//!
//! Which endpoint the authority is read off is the one the request reached, by the tree's
//! one reading of that question
//! (`service_common::data_plane_addressing::reached_emulator_endpoint`):
//!
//! * the endpoint the CALLER reached this emulator on, when that endpoint names no service —
//!   the emulator's own socket, a loopback name, or any host whose first label names none;
//! * otherwise the address this emulator is served at, because a caller that arrived on some
//!   service's endpoint host (an SDK pointed at `api.ecr-public.<region>.amazonaws.com`), or
//!   on any host inside a zone AWS publishes its own endpoints in, would be handed a host
//!   that routes to a service or to AWS, which no registry client can use.
//!
//! Nothing a client reads such a URI FOR is lost: the registry alias is reported beside it
//! (`Registry$aliases`) and in the path of both URIs, and the owning account and the
//! resource's identity are reported on the very response that carries the URI
//! (`Repository$registryId`, `Repository$repositoryArn`) — those are LABELS, built from the
//! stored record, and no endpoint changes them.
//!
//! The URI is read per RESPONSE rather than stored with the repository, so the address a
//! reader is handed is the one that reader can reach, while the record itself is untouched
//! by who read it.
//!
//! A reference a client brings IN is the other side of this: it may be written on the
//! address this registry published ([`names_this_emulator`]) or by hand on the gallery AWS
//! serves ([`names_the_public_gallery`]), so both are read as addressing this gallery
//! ([`super::gallery_pull::parse_gallery_reference`]) and the alias segment that follows says
//! WHICH public registry the reference names.

/// The two fixed labels every ECR Public registry authority AWS publishes LEADS with, as
/// `Registry$registryUri`'s "universal prefix" spells them (`public.ecr.<dns suffix>`, the
/// authority AWS serves its public gallery at).
///
/// They are fixed text rather than anything about the account or the Region: the account
/// appears only in `Repository$registryId` and the registry's alias in the path, and a
/// public registry is global, so no Region appears in the authority at all. They are what a
/// client reads to tell a public registry URI apart from a private
/// `<account>.dkr.ecr.<region>` one, which is why a reference written on such an authority is
/// read as naming this gallery ([`names_the_public_gallery`]) — on the READING side only:
/// this emulator answers on no name under that prefix, so it publishes none (see the module
/// docs).
pub const GALLERY_HOST_LABELS: [&str; 2] = ["public", "ecr"];

/// The endpoint this emulator answers THIS caller's registry requests at: the authority every
/// URI this registry publishes is built on — the registry's own URI ([`registry_uri`]) and a
/// repository's ([`repository_uri`]).
///
/// A `host[:port]`, with no scheme — which is the form both modelled URIs carry.
///
/// Two endpoints can satisfy that, and the tree's one reading of the question picks between
/// them (`service_common::data_plane_addressing::reached_emulator_endpoint`): the endpoint the
/// caller reached this emulator on when that endpoint names no service, and otherwise the
/// address this emulator is served at (see the module docs). A host inside a zone AWS
/// publishes its own endpoints in is excluded for the same reason a service's endpoint host
/// is: it resolves nowhere near this process.
pub fn announced_gallery_authority(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    use ::service_common::data_plane_addressing as addressing;

    addressing::reached_emulator_endpoint_of_parts(ctx.parts())
        .filter(|reached| !addressing::addresses_an_aws_endpoint(reached))
        .unwrap_or_else(|| addressing::emulator_endpoint_authority(ctx))
}

/// The zone a public-registry authority AWS publishes is served in — what follows the fixed
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

/// Whether `authority` names an ECR Public registry of the gallery AWS serves: an authority
/// leading with the fixed [`GALLERY_HOST_LABELS`] over a zone ([`gallery_zone_of`]).
///
/// The authority AWS publishes its gallery at ([`super::GALLERY_HOST`]) is such a name, which
/// is what lets a container reference a client wrote BY HAND be read as addressing this
/// gallery ([`super::gallery_pull::parse_gallery_reference`]) — the alias segment that
/// follows then says WHICH public registry it names, and only this registry's own alias is
/// answered.
pub fn names_the_public_gallery(authority: &str) -> bool {
    gallery_zone_of(authority).is_some()
}

/// `<endpoint>/<registry alias>` — `Registry$registryUri`, "a universal prefix and the
/// registry alias": the endpoint this registry is published at (read with
/// [`announced_gallery_authority`]) scoped by the account's primary alias.
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
/// the authority every URI this registry publishes is built on
/// ([`announced_gallery_authority`]).
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
    fn an_authority_that_names_no_public_registry_is_told_from_one_that_does() {
        // The rows span the authorities a container reference can carry: the gallery AWS
        // serves, another registry altogether, an Amazon ECR private registry host, and the
        // prefix written without a zone behind it.
        let cases: [(&str, bool); 8] = [
            (GALLERY_HOST, true),
            ("PUBLIC.ECR.AWS", true),
            ("public.ecr.gallery.example", true),
            ("public.ecr.aws:443", true),
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
        // The rows span the hosts a published URI can carry — the loopback forms a core
        // listener admits, with and without a port, and the base the emulator reports while
        // it is served on no port at all — against the registry hosts a reference names
        // instead: the gallery AWS serves, a name composed on its universal prefix (which
        // nothing resolves to this process), another registry altogether, and an Amazon ECR
        // private registry host.
        let cases: [(&str, bool); 11] = [
            ("127.0.0.1:9297", true),
            ("127.0.0.1", true),
            ("127.3.2.1:8443", true),
            ("localhost:39355", true),
            ("[::1]:9297", true),
            ("emulator.invalid", true),
            (GALLERY_HOST, false),
            ("public.ecr.localhost:9297", false),
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
        // The rows span what the composition generalises over: the endpoint forms a caller
        // reaches this emulator by (a socket, a loopback name with a port, a bracketed IPv6
        // literal, a bare host name, and one written with a trailing slash) crossed with the
        // repository-name forms the modelled `RepositoryName` pattern admits.
        let authorities = [
            "127.0.0.1:9297",
            "localhost:39355",
            "[::1]:9297",
            "emulator.invalid",
            "127.0.0.1:9297/",
        ];
        let names = ["ab", "nginx", "team.app", "project-a/nginx-web-app"];
        for authority in authorities {
            let registry = registry_uri(authority, DEFAULT_REGISTRY_ALIAS);
            let expected_registry = format!(
                "{}/{DEFAULT_REGISTRY_ALIAS}",
                authority.trim_end_matches('/')
            );
            assert_eq!(
                registry, expected_registry,
                "case {authority:?}: expected the registry URI to be the endpoint scoped by \
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
                // path it extends: an address this emulator answers on, and never the real
                // gallery host, which it does not.
                let (dialled, path) = uri
                    .split_once('/')
                    .unwrap_or_else(|| panic!("case {case:?}: {uri:?} names no path"));
                assert_eq!(
                    dialled,
                    authority.trim_end_matches('/'),
                    "case {case:?}: expected the published endpoint as the dialled one, \
                     got {dialled:?}"
                );
                assert!(
                    names_this_emulator(dialled),
                    "case {case:?}: expected an address this emulator answers on, got \
                     {dialled:?}"
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
