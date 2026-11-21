// SPDX-License-Identifier: CC0-1.0

//! The AWS Marketplace LISTING a Marketplace add-on is subscribed through, as the
//! add-on catalog holds it: the listing's own published identifiers.
//!
//! `MarketplaceInformation` carries "The product ID from the Amazon Web Services
//! Marketplace" and "The product URL from the Amazon Web Services Marketplace"
//! (`smithy_query(query="shape",shape="MarketplaceInformation")`). BOTH members are
//! the listing's own data in the AWS Marketplace catalog — the product id identifies
//! the product, and the product URL is the address of that product's page on the AWS
//! Marketplace site, which is where a subscriber accepts the seller's terms. Neither
//! is a function of the request that read the catalog, of the account that read it,
//! or of where this emulator happens to be served, so both are reference data the
//! catalog carries per listing ([`ADDON_MARKETPLACE_LISTINGS`]), and neither is
//! composed from the other.
//!
//! A product page is published at [`MARKETPLACE_PRODUCT_PAGE_ADDRESS`] followed by
//! the listing's own key, in one of the two forms AWS Marketplace publishes: the
//! `/prodview-<id>` segment of a listing addressed by its product view, or the
//! `?sku=<sku>` query of one addressed by the stock-keeping unit it is sold under
//! ([`MarketplaceProductPage`]).
//!
//! A listing whose published page this catalog cannot CITE reports NO product URL:
//! the member is optional, and an address nobody published identifies no product —
//! the same reading the sibling catalogues take of a seller's published listing
//! text, which is cited or omitted and never composed.

/// The address an AWS Marketplace product page is published under, without the
/// listing's own key.
///
/// `MarketplaceInformation$productUrl` is "The product URL from the Amazon Web
/// Services Marketplace", and a Marketplace product page lives under this address
/// followed by the key the listing is addressed by (see
/// [`MarketplaceProductPage::product_url`]).
pub const MARKETPLACE_PRODUCT_PAGE_ADDRESS: &str = "https://aws.amazon.com/marketplace/pp";

/// How one listing's product page is addressed on AWS Marketplace, or that this
/// catalog cannot cite it.
///
/// The two published forms are the Marketplace's own, carried per listing because
/// which one addresses a product is the Marketplace catalog's datum and reads
/// nothing like the add-on's name, its product id, or the other form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketplaceProductPage {
    /// Published as the product-view segment of the product page address:
    /// `…/marketplace/pp/prodview-<id>`. Carries the whole `prodview-<id>`
    /// segment, as the Marketplace catalog publishes it.
    ProductView(&'static str),
    /// Published as the stock-keeping unit the product is sold under:
    /// `…/marketplace/pp?sku=<sku>`. Carries that unit, which is the product's
    /// catalog identifier.
    StockKeepingUnit(&'static str),
    /// This catalog cannot cite the page AWS Marketplace publishes for the
    /// listing, so the add-on reports no `productUrl` at all rather than an
    /// address that identifies no product.
    Uncited,
}

impl MarketplaceProductPage {
    /// The product URL this listing publishes — the AWS Marketplace address of its
    /// product page — and `None` for a listing whose page this catalog cannot cite.
    #[must_use]
    pub fn product_url(&self) -> Option<String> {
        match self {
            Self::ProductView(product_view) => {
                Some(format!("{MARKETPLACE_PRODUCT_PAGE_ADDRESS}/{product_view}"))
            }
            Self::StockKeepingUnit(unit) => {
                Some(format!("{MARKETPLACE_PRODUCT_PAGE_ADDRESS}?sku={unit}"))
            }
            Self::Uncited => None,
        }
    }
}

/// One AWS Marketplace listing, as the add-on catalog holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddonMarketplaceListing {
    /// "The product ID from the Amazon Web Services Marketplace": the identifier
    /// of the listing in the Marketplace catalog.
    pub product_id: String,
    /// "The product URL from the Amazon Web Services Marketplace": the address of
    /// the listing's product page on the AWS Marketplace site
    /// ([`MarketplaceProductPage`]), and `None` for a listing whose published page
    /// this catalog cannot cite — which is reported as no `productUrl` member at
    /// all.
    pub product_url: Option<String>,
}
