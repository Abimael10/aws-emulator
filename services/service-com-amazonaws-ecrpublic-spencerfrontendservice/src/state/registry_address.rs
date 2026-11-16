// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS this public registry is reached at, and the two URIs it publishes on that
//! address.
//!
//! `Registry$registryUri` ("The URI of a public registry. The URI contains a universal
//! prefix and the registry alias") and `Repository$repositoryUri` ("The URI for the
//! repository. You can use this URI for container image `push` and `pull` operations") are
//! not labels a caller reads back: they are the addresses a container client DIALS to move
//! image bytes, so the requirement on them is the one every other address this emulator
//! publishes carries — THIS emulator has to answer there. AWS's own gallery host
//! ([`super::GALLERY_HOST`]) resolves to the real public registry rather than to the
//! machine this emulator runs on, so a URI naming it sends every push and pull out of the
//! emulator entirely: a client either reaches nothing or reaches an unrelated registry.
//!
//! Both URIs are therefore built on the endpoint this emulator serves the registry API at
//! ([`announced_gallery_endpoint`]), keeping the composition AWS documents — the registry's
//! URI is that endpoint scoped by the registry's alias ([`registry_uri`]), and a
//! repository's URI is the registry's URI followed by the repository name
//! ([`repository_uri`]), so a client splits and extends what it was handed exactly as it
//! does on AWS.
//!
//! Nothing a client reads such a URI FOR is lost: the registry alias is reported beside it
//! (`Registry$aliases`), and the owning account and the resource's identity are reported on
//! the very response that carries the URI (`Repository$registryId`,
//! `Repository$repositoryArn`) — those are LABELS, built from the stored record, and no
//! endpoint changes them.
//!
//! Two endpoints can satisfy "an address this emulator answers on", and
//! [`announced_gallery_endpoint`] picks between them with the tree's one reading of that
//! question (`service_common::data_plane_addressing::reached_emulator_endpoint`):
//!
//! * the endpoint the CALLER reached this emulator on, when that endpoint names no service
//!   — the emulator's own socket, a loopback name, or any host whose first label names
//!   none — because such a caller is handed the address it is already talking to;
//! * otherwise the address this emulator is served at, because a caller that arrived on
//!   some service's endpoint host (an SDK pointed at `api.ecr-public.<region>.amazonaws.com`)
//!   would be handed a host that routes to a service, which no registry client can use.
//!
//! The URI is read per RESPONSE rather than stored with the repository, so the address a
//! reader is handed is the one that reader can reach, while the record itself is untouched
//! by who read it.

/// The endpoint every URI this registry publishes is built on: the endpoint the caller
/// reached this emulator on when a registry client can use it, and the address this
/// emulator is served at otherwise (see the module docs).
///
/// A `host[:port]`, with no scheme — which is the form both modelled URIs carry.
pub fn announced_gallery_endpoint(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    ::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts(ctx.parts())
        .unwrap_or_else(|| {
            ::service_common::data_plane_addressing::emulator_endpoint_authority(ctx)
        })
}

/// `<gallery endpoint>/<registry alias>` — `Registry$registryUri`, "a universal prefix and
/// the registry alias", where the prefix is the endpoint this emulator serves the registry
/// at (`gallery_endpoint`, read with [`announced_gallery_endpoint`]).
pub fn registry_uri(gallery_endpoint: &str, registry_alias: &str) -> String {
    format!(
        "{}/{registry_alias}",
        gallery_endpoint.trim_end_matches('/')
    )
}

/// `<registry URI>/<repository name>` — the URI a client pushes the repository
/// `repository_name` to and pulls it from.
///
/// The repository's own name is appended to the registry's URI exactly as AWS appends it,
/// so a client composes the same tag, digest or registry path on what it was handed.
pub fn repository_uri(registry_uri: &str, repository_name: &str) -> String {
    format!("{registry_uri}/{repository_name}")
}

/// Whether `host` — a `host[:port]` — names an address THIS emulator answers on, which is
/// what every URI this registry publishes is built on ([`announced_gallery_endpoint`]).
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
    fn an_address_this_emulator_answers_on_is_told_from_one_it_does_not() {
        // The rows span the hosts a published URI can carry — the loopback forms a core
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
        // The rows span what the composition generalises over: the endpoint form a caller
        // reaches this emulator by (a socket, a loopback name with a port, a bracketed IPv6
        // literal, a bare host name, and one written with a trailing slash) crossed with
        // the repository-name forms the modelled `RepositoryName` pattern admits.
        let endpoints = [
            "127.0.0.1:9297",
            "localhost:39355",
            "[::1]:9297",
            "emulator.invalid",
            "127.0.0.1:9297/",
        ];
        let names = ["ab", "nginx", "team.app", "project-a/nginx-web-app"];
        for endpoint in endpoints {
            let registry = registry_uri(endpoint, DEFAULT_REGISTRY_ALIAS);
            let expected_registry = format!(
                "{}/{DEFAULT_REGISTRY_ALIAS}",
                endpoint.trim_end_matches('/')
            );
            assert_eq!(
                registry, expected_registry,
                "case {endpoint:?}: expected the registry URI to be the endpoint scoped by \
                 the alias, got {registry:?}"
            );
            for name in names {
                let uri = repository_uri(&registry, name);
                let case = format!("{endpoint} {name}");
                assert_eq!(
                    uri,
                    format!("{registry}/{name}"),
                    "case {case:?}: expected the repository URI under the registry URI, \
                     got {uri:?}"
                );
                // The host half is the address the client dials, and the rest is the path
                // it extends: never the real gallery host, which this emulator does not
                // answer on.
                let (authority, path) = uri
                    .split_once('/')
                    .unwrap_or_else(|| panic!("case {case:?}: {uri:?} names no path"));
                assert_eq!(
                    authority,
                    endpoint.trim_end_matches('/'),
                    "case {case:?}: expected the endpoint as the dialled authority, \
                     got {authority:?}"
                );
                assert_eq!(
                    path,
                    format!("{DEFAULT_REGISTRY_ALIAS}/{name}"),
                    "case {case:?}: expected the alias-scoped repository path, got {path:?}"
                );
                assert_ne!(
                    authority, GALLERY_HOST,
                    "case {case:?}: a published URI must not name the real gallery host"
                );
            }
        }
    }
}
