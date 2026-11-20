// SPDX-License-Identifier: CC0-1.0

//! The AWS Marketplace LISTING a Marketplace add-on is subscribed through: the
//! listing's own published data, the address this emulator serves that listing at,
//! and the page a fetch of that address renders.
//!
//! `MarketplaceInformation` carries "The product ID from the Amazon Web Services
//! Marketplace" and "The product URL from the Amazon Web Services Marketplace"
//! (`smithy_query(query="shape",shape="MarketplaceInformation")`). The product id
//! IDENTIFIES the listing, so it is reference data the catalog carries per listing
//! ([`ADDON_MARKETPLACE_LISTINGS`]); the product URL is an address a SUBSCRIBER
//! FOLLOWS — it is where the seller's terms are accepted, which is the step that
//! gates installing a Marketplace add-on — so it has to lead back to this
//! emulator.
//!
//! AWS publishes that address on the AWS Marketplace's own host, which this
//! emulator neither owns nor is reached on, so a URL on that host is one a client
//! can render but never dereference. What the catalog publishes instead is an
//! address of its OWN: the one endpoint this emulator serves its whole surface at
//! ([`announced_listing_base`]) followed by the path minted here, which
//! `crate::service` answers with the listing the catalog holds for it.
//!
//! The listing's identity still decides which address it is: the path carries the
//! stock-keeping UNIT the listing is sold under ([`ADDON_MARKETPLACE_LISTINGS`]),
//! which is the listing's own datum and not its product id, so the two members of
//! `MarketplaceInformation` stay two pieces of published data rather than one
//! computed from the other.

use super::*;

/// One AWS Marketplace listing, as the add-on catalog holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddonMarketplaceListing {
    /// "The product ID from the Amazon Web Services Marketplace": the identifier
    /// of the listing, in the `prod-<id>` form an AWS Marketplace product
    /// identifier takes.
    pub product_id: String,
    /// The stock-keeping unit the listing is sold under, which is the key this
    /// emulator addresses the listing's own published page by
    /// ([`addon_marketplace_listing_path`]) — this catalog's own, not recorded AWS
    /// data; see [`ADDON_MARKETPLACE_LISTINGS`].
    pub listing_unit: String,
}

/// The segment that names this service's add-on listing plane, behind the reserved
/// namespace segment every plane this emulator serves at its own front door is
/// addressed under
/// (`service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE`):
/// `/_aws/eks-addon-marketplace-listings/{listing unit}`.
///
/// Shared by the URL the catalog publishes and by the route that answers there, so
/// the address this service publishes is the address it serves.
pub const ADDON_MARKETPLACE_LISTING_PLANE: &str = "eks-addon-marketplace-listings";

/// The media type a listing page is served as: a web page, from this emulator's own
/// origin.
pub const ADDON_MARKETPLACE_LISTING_MEDIA_TYPE: &str = "text/html; charset=utf-8";

/// The request path the listing sold under `listing_unit` is served at.
///
/// A unit needs no escaping: every one the catalog carries is lower-case
/// alphanumeric ([`ADDON_MARKETPLACE_LISTINGS`]), so each character is already a
/// path character.
#[must_use]
pub fn addon_marketplace_listing_path(listing_unit: &str) -> String {
    format!(
        "/{}/{ADDON_MARKETPLACE_LISTING_PLANE}/{listing_unit}",
        ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE
    )
}

/// Where a subscriber reaches the listing sold under `listing_unit`: the endpoint
/// THIS emulator is served at (`endpoint_base`, as [`announced_listing_base`]
/// reports it) followed by that listing's own path.
#[must_use]
pub fn addon_marketplace_listing_url(endpoint_base: &str, listing_unit: &str) -> String {
    format!(
        "{}{}",
        endpoint_base.trim_end_matches('/'),
        addon_marketplace_listing_path(listing_unit)
    )
}

/// The scheme an announced address carries: this emulator serves its whole surface
/// over TLS, so a page of its own is fetched over TLS as well.
const ANNOUNCED_SCHEME: &str = "https";

/// The base URL a listing is ANNOUNCED on for the request `ctx` is answering: the
/// one endpoint this emulator serves its whole surface at.
///
/// Read through the tree's one statement of that address — the front door's own
/// published base (`service_common::data_plane_addressing::emulator_endpoint_authority`,
/// "the `host[:port]` of the ONE endpoint this emulator serves its whole surface at
/// … what a DISCOVERY operation publishes for a plane the client then DIALS") —
/// because a product URL is exactly that: an address the caller is told to open, on
/// a plane served at the front door rather than on a listener of its own.
///
/// It does not depend on which host the request that read the catalog carried, so
/// every reader of one listing is told the same address — and an address minted for
/// a request that reached this emulator through another service's in-process call
/// is one that service's own caller can reach too.
#[must_use]
pub fn announced_listing_base(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    format!(
        "{ANNOUNCED_SCHEME}://{}",
        ::service_common::data_plane_addressing::emulator_endpoint_authority(ctx)
    )
}

/// The listing unit a request `uri` addresses, for a path of the form
/// [`addon_marketplace_listing_path`] mints, and `None` for every other path.
///
/// The inverse of the minting above, read through the shared addressing module, so
/// an address this service published resolves back to the listing behind it.
/// Nothing below the unit's own segment is this route: a listing is one document,
/// not a path space.
#[must_use]
pub fn addon_marketplace_listing_route(uri: &str) -> Option<&str> {
    use ::service_common::data_plane_addressing::{first_segment, EMULATOR_PATH_NAMESPACE};
    // The path alone, read through the tree's one reader of a request target (which is
    // absolute for the form an SDK sends and relative for a browser's), so this route
    // splits a URI exactly as every other plane's does.
    let (namespace, rest) = first_segment(::service_common::uri_path(uri))?;
    if namespace != EMULATOR_PATH_NAMESPACE {
        return None;
    }
    let (plane, rest) = first_segment(rest)?;
    if plane != ADDON_MARKETPLACE_LISTING_PLANE {
        return None;
    }
    let (listing_unit, below) = first_segment(rest)?;
    (!listing_unit.is_empty() && below == "/").then_some(listing_unit)
}

impl ComAmazonawsEksAwswesleyfrontendState {
    /// The add-on whose listing is sold under `listing_unit`, or `None` for a unit
    /// the catalog sells nothing under — so only an address this service published
    /// resolves to a listing.
    #[must_use]
    pub fn addon_of_marketplace_listing(&self, listing_unit: &str) -> Option<&AddonCatalogRecord> {
        self.addon_catalog.iter().find(|addon| {
            addon
                .marketplace_listing
                .as_ref()
                .is_some_and(|listing| listing.listing_unit == listing_unit)
        })
    }

    /// The page this emulator serves at the address the listing sold under
    /// `listing_unit` is published at, or `None` for every other unit.
    ///
    /// A READ of the catalog: the page restates the published entry, so opening a
    /// listing changes nothing a later caller sees.
    #[must_use]
    pub fn addon_marketplace_listing_page(&self, listing_unit: &str) -> Option<String> {
        let addon = self.addon_of_marketplace_listing(listing_unit)?;
        let listing = addon.marketplace_listing.as_ref()?;
        Some(listing_page(addon, listing))
    }
}

/// The page a fetch of a listing's published address renders: the listing the
/// catalog holds — the product it sells, the seller that publishes it, and the
/// product identifier `DescribeAddonVersions` reports beside this address.
///
/// Every value comes from the catalog entry, so the page a subscriber opens cannot
/// disagree with what `DescribeAddonVersions` reports about the same add-on. Each
/// value is written through the escaping of the `html-escape` crate, so no stored
/// string can open or close an element of the document it is written into.
fn listing_page(addon: &AddonCatalogRecord, listing: &AddonMarketplaceListing) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>{title}</title>\n</head>\n<body>\n<h1>AWS Marketplace listing</h1>\n\
         <h2>{addon_name}</h2>\n\
         <dl>\n<dt>Product ID</dt><dd>{product_id}</dd>\n\
         <dt>Stock-keeping unit</dt><dd>{unit}</dd>\n\
         <dt>Seller</dt><dd>{publisher}</dd>\n\
         <dt>Add-on name</dt><dd>{addon_name}</dd>\n\
         <dt>Category</dt><dd>{addon_type}</dd>\n\
         <dt>Installs into</dt><dd>{namespace}</dd>\n</dl>\n</body>\n</html>\n",
        title = escaped(&format!("{} — AWS Marketplace", addon.addon_name)),
        addon_name = escaped(&addon.addon_name),
        product_id = escaped(&listing.product_id),
        unit = escaped(&listing.listing_unit),
        publisher = escaped(&addon.publisher),
        addon_type = escaped(&addon.addon_type),
        namespace = escaped(&addon.default_namespace),
    )
}

/// The page a fetch of an address of this shape that names no published listing
/// renders: a stock-keeping unit this catalog sells nothing under.
#[must_use]
pub fn addon_marketplace_listing_not_found_page() -> String {
    "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
     <title>AWS Marketplace listing</title>\n</head>\n<body>\n\
     <h1>No such listing</h1>\n</body>\n</html>\n"
        .to_string()
}

/// `value` as text inside an HTML element, through the `html-escape` crate's own
/// escaping.
fn escaped(value: &str) -> String {
    ::html_escape::encode_safe(value).into_owned()
}
