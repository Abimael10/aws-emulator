// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS this public registry PUBLISHES — for the registry itself and for every
//! repository in it — and the reading of a gallery host a client wrote a reference on.
//!
//! # What the two published URIs have to be
//!
//! `Registry$registryUri` is "The URI of a public registry. The URI contains a universal
//! prefix and the registry alias", and `Repository$repositoryUri` is "The URI for the
//! repository. You can use this URI for container image `push` and `pull` operations". So
//! each is read two ways at once: a container client DIALS it to move image bytes, and a
//! client PARSES it for the registry that holds the repository — the fixed `public`.`ecr`
//! labels ([`GALLERY_HOST_LABELS`]) are what tell a public registry's URI apart from a
//! private `<account>.dkr.ecr.<region>` one, and the alias in the path is what a consumer
//! recovers to compose an unauthenticated pull URI of its own with.
//!
//! So the authority each one carries is the public-registry host layout AWS publishes — the
//! fixed labels over a DNS zone ([`gallery_host_in_zone`]) — over the zone this emulator may
//! announce a name of its own in, carrying the port that caller reaches this emulator on
//! ([`announced_gallery_authority`]), and the composition AWS documents is kept on it: the
//! registry's URI is that authority scoped by the registry's own alias ([`registry_uri`]) —
//! the "universal prefix and the registry alias" — and a repository's URI is the registry's
//! URI followed by the repository name ([`repository_uri`]), so a client splits and extends
//! what it was handed exactly as it does on AWS, and the address it dials still leads back to
//! this process.
//!
//! The zone AWS serves its OWN gallery in ([`AWS_GALLERY_ZONE`], which gives
//! [`super::GALLERY_HOST`]) is NOT a zone this emulator may announce in, and neither is any
//! zone AWS publishes its own endpoints in: a name under either resolves to the real public
//! registry or to other genuine AWS infrastructure rather than to the machine this emulator
//! runs on, so publishing one sends every push and pull out of the emulated environment
//! entirely. The zone of the address this emulator is REACHED at is such a zone, which is why
//! the workspace's one reading of that question
//! (`service_common::data_plane_addressing::announced_zone_of_authority`) picks it — the
//! loopback zone RFC 6761 reserves for the interface the core binds on, where that address is
//! an address rather than a name, and a resolver answers every name under that zone with that
//! same interface ([`names_this_emulator`]).
//!
//! Which endpoint the zone and the port are read off is the one the request reached
//! ([`announced_gallery_endpoint`]):
//!
//! * the endpoint the CALLER reached this emulator on, whenever this gallery answers a
//!   request addressed there ([`answers_gallery_requests_at`]) — the emulator's own socket, a
//!   loopback name, any host whose first label names no service, and a public-registry
//!   authority this service itself announced, which a client then dialled back;
//! * otherwise the address this emulator is served at, because a caller that arrived on some
//!   service's endpoint host (an SDK pointed at `api.ecr-public.<region>.amazonaws.com`), on
//!   any host inside a zone AWS publishes its own endpoints in, or on the gallery AWS serves
//!   itself, would otherwise be handed a host that routes to a service or to AWS, which no
//!   registry client can use.
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
//! # Reading a gallery host a client wrote
//!
//! A reference a client brings IN is the other side of this. It may be written on the
//! authority this registry published, by hand on the gallery AWS serves
//! ([`names_the_public_gallery`] reads both, which differ only in the zone they are served
//! in), or on the endpoint a client pointed straight at this emulator
//! ([`names_this_emulator`]), so all of them are read as addressing this gallery
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
/// `<account>.dkr.ecr.<region>` one, which is why they LEAD every authority this registry
/// publishes ([`gallery_host_in_zone`]) and why a reference written on such an authority is
/// read as naming this gallery ([`names_the_public_gallery`]).
pub const GALLERY_HOST_LABELS: [&str; 2] = ["public", "ecr"];

/// The zone AWS serves its OWN public gallery in — what follows the fixed
/// [`GALLERY_HOST_LABELS`] in [`super::GALLERY_HOST`].
///
/// It is a zone this emulator must never announce a name in: such a name resolves to the real
/// public registry rather than to this process (see the module docs), so an endpoint served
/// there is read as naming no endpoint of this emulator ([`answers_gallery_requests_at`]).
pub const AWS_GALLERY_ZONE: &str = "aws";

/// `public.ecr.<zone>` — a public-registry host in `zone`: the authority layout AWS publishes
/// for a public registry, with the zone left open because the registry this emulator serves is
/// announced in a zone this emulator answers in ([`announced_gallery_authority`]), while AWS's
/// own gallery is served in [`AWS_GALLERY_ZONE`].
///
/// The ONE composer of such a host, so the authority this service announces and the authority
/// AWS publishes cannot drift in their labels or their order, and the one reading of such a
/// host ([`gallery_zone_of`]) reads either back.
pub fn gallery_host_in_zone(zone: &str) -> String {
    let [public, ecr] = GALLERY_HOST_LABELS;
    format!("{public}.{ecr}.{zone}")
}

/// The AUTHORITY every URI this registry publishes is built on: a public-registry host
/// ([`gallery_host_in_zone`]) in the zone this emulator may announce a name of its own in,
/// carrying the port that caller reaches this emulator on.
///
/// A `host[:port]`, with no scheme — which is the form both modelled URIs carry — so a client
/// splits it from the path exactly as it splits the value AWS publishes.
///
/// The zone and the port are those of the endpoint this emulator answers THAT caller at
/// ([`announced_gallery_endpoint`]), so the published name resolves to the interface this
/// emulator is bound on and the port it is served at. An endpoint that is ITSELF an authority
/// this service announced keeps the zone it already carries, so a client that dialled the
/// published value is handed that same value back rather than a second host composed on top
/// of it.
pub fn announced_gallery_authority(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    gallery_authority_on(&announced_gallery_endpoint(ctx))
}

/// [`announced_gallery_authority`] for an endpoint already read: the public-registry host in
/// that endpoint's announceable zone ([`announced_gallery_zone`]), carrying that endpoint's
/// own port.
pub fn gallery_authority_on(announced_endpoint: &str) -> String {
    let host = gallery_host_in_zone(&announced_gallery_zone(announced_endpoint));
    match ::service_common::data_plane_addressing::addressed_port(Some(announced_endpoint)) {
        Some(port) => format!("{host}:{port}"),
        None => host,
    }
}

/// The endpoint this emulator answers THIS caller's gallery requests at: the endpoint the
/// authority every URI this registry publishes is anchored on — its zone and its port.
///
/// Two endpoints can satisfy that (see the module docs): the endpoint the CALLER reached this
/// emulator on, whenever this gallery answers a request addressed there
/// ([`answers_gallery_requests_at`]), and otherwise the address this emulator is served at,
/// taken whole so the reported address stays dialable.
pub fn announced_gallery_endpoint(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    let parts = ctx.parts();
    ::service_common::data_plane_addressing::addressed_host(
        &parts.uri.to_string(),
        parts
            .headers
            .get(::http::header::HOST)
            .and_then(|value| value.to_str().ok()),
    )
    .filter(|reached| answers_gallery_requests_at(reached))
    .unwrap_or_else(|| {
        ::service_common::data_plane_addressing::emulator_endpoint_authority(ctx)
    })
}

/// Whether a gallery request addressed to `reached` — the `host[:port]` a caller stated — is
/// one THIS emulator answers, which is what makes that endpoint's zone and port the ones a
/// published authority is built on ([`announced_gallery_endpoint`]).
///
/// Three readings, each the tree's own:
///
/// * it has to be an ADDRESS at all
///   (`service_common::data_plane_addressing::is_host_shaped`), so nothing a caller wrote that
///   is not a `host[:port]` reaches a URI this service publishes;
/// * a PUBLIC-REGISTRY authority ([`gallery_zone_of`]) is one this service itself announced
///   and a client dialled back, so it is answered here — unless it is served in a zone AWS
///   serves its own names in ([`aws_serves_the_zone`]), which is the real gallery and other
///   genuine AWS infrastructure, neither of which this process answers for;
/// * every other host must name no SERVICE
///   (`service_common::data_plane_addressing::names_a_service`): a host the front door
///   resolves to a service routes there, so the address is of no use to a registry client,
///   while a host naming none — the emulator's own socket, a loopback name — is the one
///   address that caller is known to reach.
pub fn answers_gallery_requests_at(reached: &str) -> bool {
    use ::service_common::data_plane_addressing as addressing;

    if !addressing::is_host_shaped(reached) || addressing::addresses_an_aws_endpoint(reached) {
        return false;
    }
    match gallery_zone_of(reached) {
        Some(zone) => !aws_serves_the_zone(zone),
        None => !addressing::names_a_service(reached),
    }
}

/// The zone the authority announced to a caller reached at `announced_endpoint` is served in
/// ([`gallery_authority_on`]).
///
/// The zone of that endpoint, by the workspace's one reading of which zone this emulator may
/// announce a name of its own in — except that an endpoint which is ITSELF a public-registry
/// authority (the one this service announced, which a client then dialled) is read for the
/// zone it already carries ([`gallery_zone_of`]), so the announced authority is the same value
/// however many times a client is handed it. A zone AWS serves its own names in
/// ([`aws_serves_the_zone`]) is never such a zone: nothing here answers for a name AWS serves.
fn announced_gallery_zone(announced_endpoint: &str) -> String {
    gallery_zone_of(announced_endpoint)
        .filter(|zone| !aws_serves_the_zone(zone))
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| {
            ::service_common::data_plane_addressing::announced_zone_of_authority(
                announced_endpoint,
            )
        })
}

/// Whether AWS serves its OWN names in `zone`: the zone its public gallery is served in
/// ([`AWS_GALLERY_ZONE`]), or one it publishes its own endpoints in — a `dnsSuffix` its
/// partitions declare, read through the tree's one list of them
/// (`service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`, whose entries carry a
/// leading label separator) and through the reading that admits a deeper name inside one.
fn aws_serves_the_zone(zone: &str) -> bool {
    use ::service_common::data_plane_addressing as addressing;

    zone.eq_ignore_ascii_case(AWS_GALLERY_ZONE)
        || addressing::addresses_an_aws_endpoint(zone)
        || addressing::AWS_ENDPOINT_DNS_SUFFIXES
            .iter()
            .any(|suffix| zone.eq_ignore_ascii_case(suffix.trim_start_matches('.')))
}

/// The zone a public-registry authority is served in — what follows the fixed
/// [`GALLERY_HOST_LABELS`] — and `None` for an authority that does not lead with them, which
/// names no public registry.
///
/// The ONE reading of such an authority, and the inverse of the one composer of one
/// ([`gallery_host_in_zone`]): the authority this service announces and the one AWS publishes
/// differ only in that zone, so both read back as naming a public registry.
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
/// Both the authority this registry PUBLISHES ([`announced_gallery_authority`]) and the one
/// AWS publishes its gallery at ([`super::GALLERY_HOST`]) are such a name, which is what lets
/// a container reference composed on either — the URI the API handed a client, or one the
/// client wrote BY HAND — be read as addressing this gallery
/// ([`super::gallery_pull::parse_gallery_reference`]) — the alias segment that
/// follows then says WHICH public registry it names, and only this registry's own alias is
/// answered.
pub fn names_the_public_gallery(authority: &str) -> bool {
    gallery_zone_of(authority).is_some()
}

/// `<authority>/<registry alias>` — `Registry$registryUri`, "a universal prefix and the
/// registry alias": the public-registry authority this registry is published on (read with
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
/// the LOOPBACK interface the core binds its listeners on
/// (`service_common::data_plane_addressing::names_the_loopback_interface`, which admits every
/// spelling of that interface and every name under the zone RFC 6761 reserves for it — so a
/// name this emulator announces in that zone, which is what the published authority carries,
/// is recognised here), the host of the base this emulator reports while it is served on no
/// port at all ([`::smithy_server_runtime::UNSERVED_URL_BASE`], a name RFC 6761 reserves so
/// nothing resolves it), and the interface an operator bound this emulator on
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
    if let Some(zone) = gallery_zone_of(host) {
        // A PUBLIC-REGISTRY authority is an address of this emulator exactly when the zone it
        // is served in is one: that is the authority this service announces (the fixed labels
        // over the zone of the endpoint this emulator is reached at, see
        // [`announced_gallery_authority`]), while the gallery AWS serves and any other
        // registry's name are served in a zone that names nothing here.
        return names_this_emulator(zone);
    }
    if ::service_common::data_plane_addressing::names_the_loopback_interface(host) {
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
            Some(AWS_GALLERY_ZONE),
            "expected {GALLERY_HOST:?} to lead with {public}.{ecr}. and name its own zone"
        );
        assert!(
            names_the_public_gallery(GALLERY_HOST),
            "expected {GALLERY_HOST:?} to name a public registry"
        );
        // The one composer of such a host and the one reading of it are inverses, so the
        // authority this service announces carries the labels AWS publishes in AWS's order.
        assert_eq!(
            gallery_host_in_zone(AWS_GALLERY_ZONE),
            GALLERY_HOST,
            "expected the composer to spell the gallery AWS serves"
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
        // listener admits, with and without a port, the base the emulator reports while it is
        // served on no port at all, and the public-registry authorities this service announces
        // over each of those zones — against the registry hosts a reference names instead: the
        // gallery AWS serves, a public-registry name served in somebody else's zone, another
        // registry altogether, and an Amazon ECR private registry host.
        let cases: [(&str, bool); 14] = [
            ("127.0.0.1:9297", true),
            ("127.0.0.1", true),
            ("127.3.2.1:8443", true),
            ("localhost:39355", true),
            ("[::1]:9297", true),
            ("emulator.invalid", true),
            ("public.ecr.localhost:9297", true),
            ("PUBLIC.ECR.LOCALHOST", true),
            ("public.ecr.emulator.invalid", true),
            (GALLERY_HOST, false),
            ("public.ecr.gallery.example", false),
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
    fn the_published_authority_is_a_public_registry_host_in_the_zone_of_the_reached_endpoint() {
        // RULE: the authority every published URI carries is the fixed `public`.`ecr` labels
        // over the zone of the endpoint this emulator answers that caller at, carrying that
        // endpoint's port. The rows span the endpoint forms a caller reaches this emulator by:
        // a socket, a socket with no port, a loopback name with another port, a bracketed IPv6
        // literal, the base reported while nothing is served, a deeper name in the loopback
        // zone, and the published authority dialled back — in either case.
        let cases: [(&str, &str); 8] = [
            ("127.0.0.1:9297", "public.ecr.localhost:9297"),
            ("127.0.0.1", "public.ecr.localhost"),
            ("localhost:39355", "public.ecr.localhost:39355"),
            ("[::1]:38001", "public.ecr.localhost:38001"),
            ("emulator.invalid", "public.ecr.emulator.invalid"),
            ("gallery.localhost:8443", "public.ecr.gallery.localhost:8443"),
            ("public.ecr.localhost:9297", "public.ecr.localhost:9297"),
            ("PUBLIC.ECR.LOCALHOST:9297", "public.ecr.localhost:9297"),
        ];
        for (endpoint, expected) in cases {
            let authority = gallery_authority_on(endpoint);
            assert_eq!(
                authority, expected,
                "case {endpoint:?}: expected the published authority {expected:?}, got \
                 {authority:?}"
            );
            // What makes it publishable at all: a client reads it as a public registry's
            // authority, and it is an address this emulator answers on rather than the
            // gallery AWS serves.
            assert!(
                names_the_public_gallery(&authority),
                "case {endpoint:?}: expected {authority:?} to name a public registry"
            );
            assert!(
                names_this_emulator(&authority),
                "case {endpoint:?}: expected {authority:?} on an address this emulator \
                 answers on"
            );
            assert_ne!(
                ::service_common::data_plane_addressing::host_only(&authority),
                GALLERY_HOST,
                "case {endpoint:?}: a published authority must not name the real gallery host"
            );
        }
    }

    #[test]
    fn an_endpoint_this_gallery_answers_on_is_told_from_one_it_does_not() {
        // The rows span the endpoints a request can arrive on: the emulator's own socket and
        // loopback names, the authority this service itself announced (dialled back), against
        // this service's own AWS endpoint host, another zone AWS publishes its endpoints in,
        // the gallery AWS serves, a public-registry name in an AWS-served zone, a host whose
        // first label names a service, and a value that is no address at all.
        let cases: [(&str, bool); 10] = [
            ("127.0.0.1:9297", true),
            ("localhost:39355", true),
            ("[::1]:38001", true),
            ("public.ecr.localhost:9297", true),
            ("api.ecr-public.us-east-1.amazonaws.com", false),
            ("api.ecr-public.us-east-1.api.aws", false),
            (GALLERY_HOST, false),
            ("public.ecr.amazonaws.com", false),
            ("dynamodb.localhost:39355", false),
            ("elsewhere.example.com/gallery", false),
        ];
        for (endpoint, expected) in cases {
            let answered = answers_gallery_requests_at(endpoint);
            assert_eq!(
                answered, expected,
                "case {endpoint:?}: expected answers_gallery_requests_at to be {expected}, \
                 got {answered}"
            );
        }
    }

    #[test]
    fn a_repository_uri_extends_the_registry_uri_it_is_held_in() {
        // The rows span what the composition generalises over: the endpoint forms a caller
        // reaches this emulator by (a socket, a loopback name with a port, a bracketed IPv6
        // literal, a bare host name, and the published authority dialled back) crossed with
        // the repository-name forms the modelled `RepositoryName` pattern admits.
        let endpoints = [
            "127.0.0.1:9297",
            "localhost:39355",
            "[::1]:9297",
            "emulator.invalid",
            "public.ecr.localhost:9297",
        ];
        let names = ["ab", "nginx", "team.app", "project-a/nginx-web-app"];
        for endpoint in endpoints {
            let authority = gallery_authority_on(endpoint);
            let registry = registry_uri(&authority, DEFAULT_REGISTRY_ALIAS);
            let expected_registry = format!("{authority}/{DEFAULT_REGISTRY_ALIAS}");
            assert_eq!(
                registry, expected_registry,
                "case {endpoint:?}: expected the registry URI to be the published authority \
                 scoped by the alias, got {registry:?}"
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
                // The authority half is the address the client dials, and the rest is the
                // path it extends: a public registry's authority on an address this emulator
                // answers on, and never the real gallery host, which it does not.
                let (dialled, path) = uri
                    .split_once('/')
                    .unwrap_or_else(|| panic!("case {case:?}: {uri:?} names no path"));
                assert_eq!(
                    dialled, authority,
                    "case {case:?}: expected the published authority as the dialled one, \
                     got {dialled:?}"
                );
                assert!(
                    names_the_public_gallery(dialled),
                    "case {case:?}: expected a public registry's authority, got {dialled:?}"
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
