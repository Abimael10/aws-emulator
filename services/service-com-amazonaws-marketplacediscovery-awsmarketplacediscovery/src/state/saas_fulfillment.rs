// SPDX-License-Identifier: CC0-1.0

//! The buyer-facing pages a SaaS fulfillment option's record names, and the address this
//! emulator serves each of them at.
//!
//! `SaasFulfillmentOption` carries two addresses a BUYER follows rather than calls:
//! `fulfillmentUrl`, "The URL of the seller's software registration landing page", and
//! `launchUrl`, "The URL that a buyer uses to launch the seller's SaaS product"
//! (`smithy_query(query="shape",shape="SaasFulfillmentOption")`). AWS serves them on the
//! seller's own hosts, which this emulator neither owns nor answers on, so what it publishes
//! is an address of its OWN: the endpoint a request reached it at
//! ([`::smithy_server_runtime::OperationContext::endpoint`]) followed by the path minted
//! here, which `crate::service` answers from the stored record. A URL published on any other
//! host is one the client can render but never dereference.
//!
//! Which of the two pages an option has is a property of its RECORD
//! ([`SaasServedPages`]), so the option announces exactly the pages the catalog holds for it
//! and no member is composed for a record that names none.

use super::*;

/// Which buyer-facing page of a SaaS fulfillment option an address names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaasPage {
    /// The seller's software registration landing page, which `fulfillmentUrl` names.
    Registration,
    /// The page a launch of the product sends the buyer to, which `launchUrl` names.
    Launch,
}

impl SaasPage {
    /// The last path segment that names this page.
    const fn segment(self) -> &'static str {
        match self {
            Self::Registration => "register",
            Self::Launch => "launch",
        }
    }

    /// The page a path segment names, and `None` for a segment naming neither.
    const fn from_segment(segment: &str) -> Option<Self> {
        match segment.as_bytes() {
            b"register" => Some(Self::Registration),
            b"launch" => Some(Self::Launch),
            _ => None,
        }
    }

    /// What the page is headed, which is what it is FOR: registering for the product, or
    /// launching it.
    const fn heading(self) -> &'static str {
        match self {
            Self::Registration => "Register for this product",
            Self::Launch => "Launch this product",
        }
    }
}

/// Which buyer-facing pages a SaaS fulfillment option's record names.
///
/// A record names none by default ([`Default`]), so an option announces a page only where
/// the catalog holds one for it — `fulfillmentUrl` and `launchUrl` are both optional members
/// and the presence of one is what a client branches on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SaasServedPages {
    /// Whether the record names a registration landing page.
    pub registration: bool,
    /// Whether the record names a launch page.
    pub launch: bool,
}

impl SaasServedPages {
    /// Whether this record names `page`.
    pub const fn names(self, page: SaasPage) -> bool {
        match page {
            SaasPage::Registration => self.registration,
            SaasPage::Launch => self.launch,
        }
    }
}

/// The segment that names this service's SaaS fulfillment plane, behind the reserved
/// namespace segment every plane this emulator serves at its own front door is addressed
/// under (`service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE`):
/// `/_aws/marketplace-discovery-saas-fulfillment/{fulfillmentOptionId}/{page}`.
///
/// Shared by the URL an option announces and by the route that answers there, so the address
/// this service publishes is the address it serves.
pub const SAAS_FULFILLMENT_PLANE_SEGMENT: &str = "marketplace-discovery-saas-fulfillment";

/// The media type a buyer-facing page is served as: a web page, from this emulator's own
/// origin.
pub const SAAS_PAGE_MEDIA_TYPE: &str = "text/html; charset=utf-8";

/// The request path `page` of the fulfillment option `fulfillment_option_id` is served at.
pub fn saas_page_path(fulfillment_option_id: &str, page: SaasPage) -> String {
    format!(
        "/{}/{SAAS_FULFILLMENT_PLANE_SEGMENT}/{fulfillment_option_id}/{}",
        ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE,
        page.segment()
    )
}

/// Where a buyer reaches `page` of the fulfillment option `fulfillment_option_id`: the
/// endpoint THIS emulator is served at (`endpoint_base`) followed by that page's own path.
pub fn saas_page_url(endpoint_base: &str, fulfillment_option_id: &str, page: SaasPage) -> String {
    format!(
        "{}{}",
        endpoint_base.trim_end_matches('/'),
        saas_page_path(fulfillment_option_id, page)
    )
}

/// The base URL the pages of a fulfillment option are ANNOUNCED on, for the request `ctx` is
/// answering: the address that request reached this emulator at.
///
/// The reading is `service_common::data_plane_addressing`'s, in the order every address this
/// emulator announces for a plane served at its own front door is taken in, so this service
/// cannot drift from the others on which address that is: the emulator's OWN endpoint the
/// request addressed (a caller that resolves nothing else), else the port this process is
/// served on (so an address minted through an in-process call of another service is one that
/// service's caller can reach too), else the base `OperationContext::endpoint` reports for an
/// emulator served on no port at all. The scheme is the one every listener of this process
/// presents a certificate for, whatever the endpoint base is written as.
pub fn announced_page_base(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    use ::service_common::data_plane_addressing::{
        reached_emulator_endpoint_of_parts, served_endpoint_authority, url_authority,
    };
    let base = ctx.endpoint();
    let authority = reached_emulator_endpoint_of_parts(ctx.parts())
        .or_else(|| served_endpoint_authority(base.as_str()))
        .unwrap_or_else(|| url_authority(base.as_str()).trim_end_matches('/').to_string());
    format!("https://{authority}")
}

/// The fulfillment option and page a request `uri` addresses, for a path of the form
/// [`saas_page_path`] mints, and `None` for every other path.
///
/// The inverse of the minting above, read through the shared addressing module, so a URL
/// this service published resolves back to the record behind it. Nothing below the page's own
/// segment is this route: a page is one document, not a path space.
pub fn saas_page_route(uri: &str) -> Option<(&str, SaasPage)> {
    use ::service_common::data_plane_addressing::{first_segment, EMULATOR_PATH_NAMESPACE};
    let (namespace, rest) = first_segment(request_target_path(uri))?;
    if namespace != EMULATOR_PATH_NAMESPACE {
        return None;
    }
    let (plane, rest) = first_segment(rest)?;
    if plane != SAAS_FULFILLMENT_PLANE_SEGMENT {
        return None;
    }
    let (fulfillment_option_id, rest) = first_segment(rest)?;
    let (page, below) = first_segment(rest)?;
    if fulfillment_option_id.is_empty() || below != "/" {
        return None;
    }
    SaasPage::from_segment(page).map(|page| (fulfillment_option_id, page))
}

/// The PATH of a request target, whether it arrived in origin form (`/a/b?q`) or in absolute
/// form (`https://host/a/b?q`), which is how a request that carries its endpoint in the URI
/// spells the same address.
fn request_target_path(uri: &str) -> &str {
    let path = uri.split(['?', '#']).next().unwrap_or(uri);
    match path.split_once("://") {
        Some((_scheme, authority_and_path)) => authority_and_path
            .find('/')
            .map_or("/", |at| &authority_and_path[at..]),
        None => path,
    }
}

impl ComAmazonawsMarketplacediscoveryAwsmarketplacediscoveryState {
    /// The page this emulator serves at the address `page` of the SaaS fulfillment option
    /// `fulfillment_option_id` is published at.
    ///
    /// `None` when the catalog holds no option of that id, when the option it holds is not a
    /// SaaS one, or when that option's record names no such page — so only an address this
    /// service actually published answers with a page.
    ///
    /// A READ of the catalog: the page restates the stored record, so opening it changes
    /// nothing a later caller sees.
    pub fn saas_fulfillment_page(
        &self,
        fulfillment_option_id: &str,
        page: SaasPage,
    ) -> Option<String> {
        let (product, record) = self.products.values().find_map(|product| {
            product
                .fulfillment_options
                .iter()
                .find(|record| record.id() == fulfillment_option_id)
                .map(|record| (product, record))
        })?;
        if !record.served_pages.names(page) {
            return None;
        }
        let FulfillmentOption::saasFulfillmentOption(option) = &record.default else {
            return None;
        };
        Some(saas_page(product, option, page))
    }
}

/// The page a fetch of one of a SaaS option's published addresses renders: the product the
/// option fulfils and the option itself, as the catalog holds them right now.
///
/// Every value comes from the stored record, so the page a buyer opens cannot disagree with
/// what `ListFulfillmentOptions` reports about the same option. Each value is written through
/// the escaping of the `html-escape` crate, so no stored string can open or close an element
/// of the document it is written into.
fn saas_page(product: &Product, option: &SaasFulfillmentOption, page: SaasPage) -> String {
    let name = &product.text.text(DEFAULT_LOCALE).name;
    let instructions = option
        .usage_instructions
        .as_deref()
        .map(|instructions| format!("<p>{}</p>\n", escaped(instructions)))
        .unwrap_or_default();
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>{title}</title>\n</head>\n<body>\n<h1>{heading}</h1>\n<h2>{name}</h2>\n\
         <dl>\n<dt>Product</dt><dd>{product_id}</dd>\n\
         <dt>Fulfillment option</dt><dd>{option_id}</dd>\n\
         <dt>Fulfilled as</dt><dd>{display_name}</dd>\n\
         <dt>Quick launch</dt><dd>{quick_launch}</dd>\n</dl>\n{instructions}</body>\n</html>\n",
        title = escaped(&format!("{name} — {}", option.fulfillment_option_display_name)),
        heading = page.heading(),
        name = escaped(name),
        product_id = escaped(&product.product_id),
        option_id = escaped(&option.fulfillment_option_id),
        display_name = escaped(&option.fulfillment_option_display_name),
        quick_launch = escaped(&option.quick_launch),
    )
}

/// The page a fetch of an address of this shape that names no published page renders: a
/// fulfillment option this catalog does not hold, or one whose record names no such page.
pub fn saas_page_not_found_page() -> String {
    "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
     <title>AWS Marketplace fulfillment</title>\n</head>\n<body>\n\
     <h1>No such fulfillment page</h1>\n</body>\n</html>\n"
        .to_string()
}

/// `value` as text inside an HTML element, through the `html-escape` crate's own escaping.
fn escaped(value: &str) -> String {
    ::html_escape::encode_safe(value).into_owned()
}
