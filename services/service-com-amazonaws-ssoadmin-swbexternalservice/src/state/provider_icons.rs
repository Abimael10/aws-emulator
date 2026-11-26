// SPDX-License-Identifier: CC0-1.0

//! The portal ICON of an application-provider catalog entry: where a client fetches it, and
//! the image this emulator serves there.
//!
//! `DisplayData$IconUrl` is "a URL that points to an icon that represents the application
//! provider" — not a label, but an absolute address the client that read the entry fetches
//! NEXT, to render the provider's portal tile. IAM Identity Center publishes it on its own
//! static asset host, which this emulator does not serve: an address handed to a caller has
//! to lead back to the endpoint that caller is already talking to, or the fetch either leaves
//! the emulator for the real AWS CDN (which a sandbox with no egress cannot reach) or renders
//! nothing at all.
//!
//! So the catalog stores no absolute URL. The entry carries only WHETHER it publishes an icon
//! ([`super::BuiltInApplicationProvider::publishes_icon`]), and a read composes the address
//! from the endpoint the request arrived at plus the entry's own id ([`provider_icon_url`]) —
//! under the reserved path namespace every data plane this emulator serves is addressed in
//! (`service_common::data_plane_addressing`), so the path collides with no modelled route.
//! The image itself is served at exactly that path (`crate::service`, over
//! [`provider_icon_image`]), anonymously, as the published icon of a provider catalog is
//! fetched.
//!
//! The emulator holds no artwork for an AWS-published catalog entry, so what answers is the
//! image every service of this workspace serves for a record whose own picture it does not
//! hold (`service_common::record_identicon`), derived from the entry's id: the same image on
//! every fetch, a different one per provider, and a real PNG a client's decoder reads.

use super::*;

/// The segment under the emulator's reserved data-plane namespace that names this service's
/// application-provider icons: a request path of the form
/// `/_aws/<this>/<provider id>/icon.png`.
///
/// Shared by the URL a read hands out ([`provider_icon_url`]) and by the route that serves it
/// ([`provider_icon_route`], read in `crate::service`), so an address this service published
/// and the path it is served on cannot disagree.
pub const PROVIDER_ICON_PLANE: &str = "sso-application-provider-icons";

/// The last path segment of every icon address. The entry's own id is what sits between the
/// plane and this segment, so one provider's icon cannot be addressed as another's.
const PROVIDER_ICON_FILE_NAME: &str = "icon.png";

/// The media type a provider icon is served as, and the format the shared renderer writes: a
/// PNG, which is what the `.png` address names
/// (`service_common::record_identicon::IDENTICON_MEDIA_TYPE`).
pub const PROVIDER_ICON_MEDIA_TYPE: &str = ::service_common::record_identicon::IDENTICON_MEDIA_TYPE;

/// The path this emulator serves the icon of the catalog entry `provider_id` at.
///
/// The ONE statement of that layout: [`provider_icon_url`] addresses this path on the
/// endpoint a request arrived at and [`provider_icon_route`] reads it back, so an address
/// handed out and an address served cannot drift apart.
///
/// The id is written as it stands, which is a round trip: a provider id is a service slug or
/// `catalog/<Name>`, and the model's `ApplicationProviderArn` pattern admits only
/// `[a-zA-Z0-9-/]` in it, so every character it can carry is already a path character and the
/// `/` of a `catalog/` id is simply a further segment the route reads back over.
pub fn provider_icon_path(provider_id: &str) -> String {
    format!(
        "/{}/{PROVIDER_ICON_PLANE}/{provider_id}/{PROVIDER_ICON_FILE_NAME}",
        ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE
    )
}

/// `DisplayData$IconUrl` for the catalog entry `provider_id`: [`provider_icon_path`] on the
/// endpoint this request arrived at (`endpoint_base`, what `ctx.endpoint()` answers).
///
/// Composed per read rather than stored, because the host is the one this emulator is reached
/// on at the moment of the read, which the catalog cannot know. The icon the address leads to
/// is the entry's own either way ([`provider_icon_image`]).
pub fn provider_icon_url(endpoint_base: &str, provider_id: &str) -> String {
    format!(
        "{}{}",
        endpoint_base.trim_end_matches('/'),
        provider_icon_path(provider_id)
    )
}

/// The provider id whose icon `uri` addresses, for a path of the form
/// [`provider_icon_path`] builds, and `None` for every other path.
///
/// A provider id may itself carry `/` (a `catalog/<Name>` entry), so everything between this
/// plane and the trailing file name is the id. Nothing about the catalog is consulted here:
/// WHICH entry an address may reach is decided by the catalog itself
/// ([`provider_icon_image`]), so a path naming no entry is answered exactly as one naming an
/// entry that publishes no icon.
pub fn provider_icon_route(uri: &str) -> Option<&str> {
    let path = uri.split(['?', '#']).next().unwrap_or(uri);
    let path = match path.split_once("://") {
        Some((_scheme, authority_and_path)) => authority_and_path
            .find('/')
            .map(|at| &authority_and_path[at..])?,
        None => path,
    };
    let (namespace, rest) = ::service_common::data_plane_addressing::first_segment(path)?;
    if namespace != ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE {
        return None;
    }
    let (plane, rest) = ::service_common::data_plane_addressing::first_segment(rest)?;
    if plane != PROVIDER_ICON_PLANE {
        return None;
    }
    let provider_id = rest
        .strip_prefix('/')?
        .strip_suffix(PROVIDER_ICON_FILE_NAME)?
        .strip_suffix('/')?;
    (!provider_id.is_empty()).then_some(provider_id)
}

/// The icon image the address of the catalog entry `provider_id` leads to, or `None` when
/// this emulator serves no image there — an id the catalog does not hold, and an entry the
/// catalog publishes no icon for, which are answered alike.
///
/// Resolved against the catalog the reporting operations read
/// ([`BUILT_IN_APPLICATION_PROVIDERS`]), so an address is served an image exactly while
/// `DescribeApplicationProvider` reports an `IconUrl` for the entry it belongs to, and the
/// image is rendered from the STORED entry's id rather than from the requested text.
pub fn provider_icon_image(provider_id: &str) -> Option<Vec<u8>> {
    let provider = BUILT_IN_APPLICATION_PROVIDERS
        .iter()
        .find(|provider| provider.id == provider_id)
        .filter(|provider| provider.publishes_icon)?;
    ::service_common::record_identicon::identicon_png(provider.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first eight bytes of every PNG file (RFC 2083 section 3.1).
    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    #[test]
    fn an_icon_url_and_the_route_that_serves_it_read_the_same_path() {
        // The rows span what the addressing generalises over: both id forms, an id whose
        // `catalog/` segment carries the `/` the route reads back over, an endpoint base
        // written with a trailing slash, and a base on a host with no port.
        let cases: [(&str, &str); 5] = [
            ("https://127.0.0.1:9297", "glue"),
            ("https://127.0.0.1:9297", "catalog/DuoNetworkGateway"),
            ("https://127.0.0.1:9297/", "catalog/DruvaInSync"),
            ("https://emulator.invalid", "custom"),
            ("https://[::1]:443", "catalog/15Five"),
        ];
        for (base, provider_id) in cases {
            let case = format!("{base} {provider_id}");
            let url = provider_icon_url(base, provider_id);
            assert!(
                url.starts_with(base.trim_end_matches('/')),
                "case {case:?}: expected the URL on the endpoint base, got {url:?}"
            );
            assert_eq!(
                provider_icon_route(&url),
                Some(provider_id),
                "case {case:?}: expected the route to read the provider back, got {:?}",
                provider_icon_route(&url)
            );
        }
    }

    #[test]
    fn a_path_that_names_no_provider_icon_is_not_claimed() {
        // The rows span the paths this route must leave alone: another plane's, the modelled
        // control-plane route, this plane with no provider under it, a path with no file name
        // segment, and the path the real asset host serves an icon on.
        let cases: [&str; 6] = [
            "/_aws/ecr-public-logos/123456789012/nginx/icon.png",
            "/",
            "/_aws/sso-application-provider-icons",
            "/_aws/sso-application-provider-icons/icon.png",
            "/_aws/sso-application-provider-icons/glue",
            "/app-d77638588eff9057/icons/default.png",
        ];
        for path in cases {
            let route = provider_icon_route(path);
            assert!(
                route.is_none(),
                "case {path:?}: expected no provider icon, got {route:?}"
            );
        }
    }

    #[test]
    fn an_image_is_served_for_exactly_the_entries_that_publish_one() {
        // Every entry of the catalog, so both sides of the rule are covered whichever entries
        // publish an icon, plus two ids the catalog does not hold at all.
        let mut images: Vec<Vec<u8>> = Vec::new();
        for provider in BUILT_IN_APPLICATION_PROVIDERS {
            let case = provider.id;
            let image = provider_icon_image(provider.id);
            if !provider.publishes_icon {
                assert!(
                    image.is_none(),
                    "case {case:?}: the catalog publishes no icon for this entry, so none may \
                     be served, got {:?} bytes",
                    image.map(|bytes| bytes.len())
                );
                continue;
            }
            let image = image
                .unwrap_or_else(|| panic!("case {case:?}: expected the entry's icon image"));
            assert!(
                image.starts_with(&PNG_SIGNATURE),
                "case {case:?}: expected PNG bytes, got {} bytes starting {:?}",
                image.len(),
                image.get(..PNG_SIGNATURE.len())
            );
            assert_eq!(
                provider_icon_image(provider.id).as_ref(),
                Some(&image),
                "case {case:?}: the same entry must serve the same image on every fetch"
            );
            assert!(
                !images.contains(&image),
                "case {case:?}: every entry must serve an image of its own"
            );
            images.push(image);
        }
        assert!(
            images.len() > 1,
            "the catalog must publish an icon for more than one entry for the per-entry rule \
             to be observable, got {}",
            images.len()
        );
        for unknown in ["catalog/NoSuchApplication", "no-such-service"] {
            let image = provider_icon_image(unknown);
            assert!(
                image.is_none(),
                "case {unknown:?}: the catalog holds no such entry, so no image may be served"
            );
        }
    }
}
