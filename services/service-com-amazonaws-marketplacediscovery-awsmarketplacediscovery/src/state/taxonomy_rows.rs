// SPDX-License-Identifier: CC0-1.0

//! The per-category tier of the seeded catalog: one listing in EVERY subcategory of the
//! catalog's taxonomy, each classified under a second subcategory as well.
//!
//! AWS Marketplace is a catalog far larger than one page of search results, and
//! `SearchListings` is `@paginated` with `maxResults` ranging 1..100: "[i]f nextToken is
//! returned, there are more results available"
//! (`smithy_query(query="operation",operation="SearchListings")`). A continuation token can
//! only exist when the match set is larger than the page served, so what a client can page
//! at all is decided by how many listings a query matches — for a search over the whole
//! catalog AND for one a client has refined by a facet value. This tier is what makes both
//! continuable: it holds a listing in every subcategory the taxonomy declares
//! ([`CATEGORY_TAXONOMY`]) — which fills that subcategory's ancestors too — and classifies
//! each one under a second subcategory from another part of the tree, so every category,
//! and every top-level category carrying its whole subtree's listings, matches several
//! listings rather than one.
//!
//! This tier is also what a client reaches FIRST when it discovers the catalog the
//! documented way — a default, unfiltered `SearchListings`, then a drill-down into an
//! associated product — so the multiplicity the model gives a product's fulfillment
//! ([`KIND_FULFILLMENT`]) has to be visible here: the rows whose kind of product is
//! delivered through two routes offer two fulfillment options, so a client that pages
//! `ListFulfillmentOptions` over a product it found by searching meets a list longer than
//! one page of `maxResults=1`.
//!
//! The records are this emulator's own seed data: each row's listing, product, offer, offer
//! terms and (for some rows) offer set are built by [`seed_rows`], the same path the
//! hand-written rows above go through, so no tier's records are shaped differently from
//! another's.

use super::*;

/// How far apart the two subcategories one row is classified under sit in
/// [`subcategories`]'s order.
///
/// [`pair_step`] turns it into a stride that always lands on ANOTHER subcategory, so every
/// subcategory gets two listings of this tier (its own row's and the row that paired with
/// it); this value is large enough that the pair usually sits in another top-level subtree,
/// so a listing is classified across the taxonomy the way a real one is.
const PAIR_STRIDE: usize = 17;

/// The first line of this tier's `555-01xx` support telephone block, clear of the numbers
/// the hand-written rows (`0101..0106`) and the breadth rows (`0110..`) take. The `555-01xx`
/// range is reserved for fictional use in every area code, so this tier takes the next area
/// code of [`PHONE_AREA_CODES`] each time it runs out of lines in the block.
const PHONE_BLOCK: usize = 140;

/// How many lines of the `555-01xx` block this tier takes per area code: `0140..0199`.
const PHONE_LINES: usize = 60;

/// The area codes this tier's fictional telephone numbers are taken from, in turn, so the
/// numbers stay inside the reserved `555-01xx` range however many categories the taxonomy
/// declares.
const PHONE_AREA_CODES: &[&str] = &["206", "212", "312", "415", "512"];

/// The first sequence number of this tier's id block, clear of the hand-written records
/// (`0001..`) and the breadth rows (`0101..`).
const SEQUENCE_BLOCK: usize = 201;

/// What kind of product each row is, cycled over the rows.
///
/// Marketplace listings are not all one kind of thing, and the word is part of what a
/// `searchText` keyword can match, so the catalog carries several. The kind also says how
/// the record is DELIVERED ([`KIND_FULFILLMENT`]).
const PRODUCT_KINDS: &[&str] = &[
    "Platform",
    "Suite",
    "Service",
    "Hub",
    "Workbench",
    "Toolkit",
    "Gateway",
    "Studio",
];

/// How a row of this tier is fulfilled, by the kind of product it is: `(kind, every
/// FulfillmentOptionType that kind is delivered through)`.
///
/// A record's fulfillment is a property of the record, so this tier reads it off the same
/// thing the rest of the row is derived from — what the product IS — rather than from the
/// row's position in the model's enum: seller-hosted software (a platform, a suite, a
/// service, a studio) is reached over the web and so delivered as SaaS; a hub runs in the
/// buyer's own cluster, as a container; a workbench is an image the buyer launches; a
/// toolkit is build components; a gateway is provisioned into the buyer's network from a
/// template. Spanning the whole `FulfillmentOptionType` union is [`BREADTH_ROWS`]' job,
/// which names every one of its values.
///
/// A kind delivered through MORE THAN ONE route names every one of them, because a product
/// stands in a one-to-many relation to its fulfillment options — `ListFulfillmentOptions`
/// is `@paginated` over `fulfillmentOptions` and `fulfillmentOptionSummaries` is a list of
/// the types a product offers — and software offered through two routes is the ordinary
/// case rather than the exception: a platform its seller hosts is subscribed to and called
/// as an API; a hub ships as a container image and as the Helm chart that installs it; a
/// workbench ships as a machine image and as the Image Builder component that bakes it into
/// the buyer's own image. The kinds delivered one way name one route, so this tier carries
/// single-route and multi-route products alike.
const KIND_FULFILLMENT: &[(&str, &[&str])] = &[
    ("Platform", &["SAAS", "API"]),
    ("Suite", &["SAAS"]),
    ("Service", &["SAAS"]),
    ("Hub", &["CONTAINER", "HELM"]),
    (
        "Workbench",
        &["AMAZON_MACHINE_IMAGE", "EC2_IMAGE_BUILDER_COMPONENT"],
    ),
    ("Toolkit", &["EC2_IMAGE_BUILDER_COMPONENT"]),
    ("Gateway", &["CLOUDFORMATION_TEMPLATE"]),
    ("Studio", &["SAAS"]),
];

/// How a row of this tier is PRICED, by the kind of product it is: `(kind,
/// PricingModelType, PricingUnitType)`.
///
/// A record's pricing is a property of the record — `GetListingOutput$pricingModels` is
/// "[t]he pricing models for offers associated with this listing" and `$pricingUnits` "[t]he
/// pricing units that define the billing dimensions for offers associated with this
/// listing" (`smithy_query(query="shape",shape="GetListingOutput")`) — so this tier reads it
/// off the same thing the rest of the row is derived from, what the product IS and how it is
/// delivered, rather than from the row's position in the model's enums: seller-hosted
/// software is subscribed to for the people, seats or tiers that use it; an API-reachable
/// platform is metered by the requests it answers; software that runs in the buyer's own
/// cluster or on its own hosts is metered per host; build components the buyer bakes into its
/// own images are run under licences it already owns; a toolkit a seller publishes at no
/// charge is free.
///
/// Spanning every `PricingModelType` and `PricingUnitType` the model declares is
/// [`BREADTH_ROWS`]' job, which states each row's pricing one by one.
const KIND_PRICING: &[(&str, &str, &str)] = &[
    ("Platform", "USAGE", "REQUESTS"),
    ("Suite", "CONTRACT", "USERS"),
    ("Service", "CONTRACT", "USERS"),
    ("Hub", "USAGE", "HOSTS"),
    ("Workbench", "BYOL", "HOSTS"),
    ("Toolkit", "FREE", "UNITS"),
    ("Gateway", "USAGE", "BANDWIDTH"),
    ("Studio", "CONTRACT", "TIERS"),
];

/// The key features a product of `kind` offers customers, which is what `highlights` ("A
/// list of key features that the listing offers to customers") carries for both its listing
/// and its product.
///
/// A feature is something the product DOES, so it is stated per kind of product rather than
/// composed from how the record is classified or delivered — a response already carries both
/// of those, as `categories` and `fulfillmentOptionSummaries`.
const KIND_HIGHLIGHTS: &[(&str, &[&str])] = &[
    (
        "Platform",
        &[
            "One control plane across every account you own",
            "Single sign-on with role-based access control",
            "Usage and cost reporting per team",
        ],
    ),
    (
        "Suite",
        &[
            "Modules that share one data model",
            "Approval workflows with full audit history",
            "Scheduled exports to your warehouse",
        ],
    ),
    (
        "Service",
        &[
            "Managed by the seller, with a published uptime target",
            "Private connectivity from your own VPC",
            "Hourly backups with point-in-time restore",
        ],
    ),
    (
        "Hub",
        &[
            "Runs beside your workloads in your own cluster",
            "Horizontal autoscaling under load",
            "Policy enforcement at admission time",
        ],
    ),
    (
        "Workbench",
        &[
            "Pre-installed toolchain ready at first boot",
            "Reproducible environments from a pinned manifest",
            "Notebook sessions resumable after a restart",
        ],
    ),
    (
        "Toolkit",
        &[
            "Composable components you add to your own pipeline",
            "Deterministic builds from a locked manifest",
            "Signed artefacts with provenance attestations",
        ],
    ),
    (
        "Gateway",
        &[
            "Terminates and inspects traffic at your edge",
            "Rate limiting and quota enforcement per client",
            "Failover across availability zones",
        ],
    ),
    (
        "Studio",
        &[
            "Visual authoring with versioned drafts",
            "Review and comment before publishing",
            "Publish to staging and production separately",
        ],
    ),
];

/// The `FulfillmentOptionType`s a product of `kind` is delivered through, per
/// [`KIND_FULFILLMENT`], the first of them being the route the row's prose names.
///
/// A kind this table does not name is delivered as SaaS, the way a product the catalog
/// knows nothing more about than that its seller hosts it is reached.
fn fulfillment_for_kind(kind: &str) -> &'static [&'static str] {
    KIND_FULFILLMENT
        .iter()
        .find(|(candidate, _)| *candidate == kind)
        .map_or(&["SAAS"], |(_, option_types)| *option_types)
}

/// The pricing a product of `kind` is sold under, per [`KIND_PRICING`], as
/// `(PricingModelType, PricingUnitType)`.
///
/// A kind this table does not name is sold the way seller-hosted software with no other
/// stated pricing is: a subscription contract sized in units.
fn pricing_for_kind(kind: &str) -> (&'static str, &'static str) {
    KIND_PRICING
        .iter()
        .find(|(candidate, _, _)| *candidate == kind)
        .map_or(("CONTRACT", "UNITS"), |(_, model, unit)| (*model, *unit))
}

/// The key features a product of `kind` offers, per [`KIND_HIGHLIGHTS`].
///
/// A kind this table does not name states no feature, and then the record reports none:
/// `highlights` is a list, and an empty one claims nothing rather than claiming a feature
/// the record has not stated.
fn highlights_for_kind(kind: &str) -> &'static [&'static str] {
    KIND_HIGHLIGHTS
        .iter()
        .find(|(candidate, _)| *candidate == kind)
        .map_or(&[], |(_, features)| *features)
}

/// Seed one listing (with its product, offer and terms) per subcategory of the taxonomy.
pub(super) fn seed_taxonomy_rows(
    state: &mut ComAmazonawsMarketplacediscoveryAwsmarketplacediscoveryState,
    now: i64,
) {
    let categories = subcategories();
    let count = categories.len();
    if count == 0 {
        // A taxonomy with no subcategory has no row to derive, and nothing to pair with.
        return;
    }
    let step = pair_step(count);
    let built: Vec<BuiltRow> = categories
        .iter()
        .enumerate()
        .map(|(index, category)| {
            let paired = categories[(index + step) % count];
            BuiltRow::new(index, category, paired)
        })
        .collect();
    let rows: Vec<CatalogRow<'_>> = built.iter().map(BuiltRow::row).collect();
    seed_rows(state, now, &rows);
}

/// The stride that pairs a subcategory with another one, for a taxonomy of `count`
/// subcategories.
///
/// [`PAIR_STRIDE`] itself lands back on the row's own subcategory whenever `count` divides
/// it, which would classify a listing twice under one category and leave that category with
/// a single listing — the opposite of what this tier is for. A taxonomy with ONE subcategory
/// has no other to pair with, and then the row carries its own category alone.
fn pair_step(count: usize) -> usize {
    match PAIR_STRIDE % count {
        0 => usize::from(count > 1),
        step => step,
    }
}

/// Every category of the taxonomy that sits under another one, in the taxonomy's order.
///
/// A record is classified at a SUBcategory and carries its ancestors through
/// [`classified_under`], so seeding the subcategories is what fills the top-level
/// categories too.
fn subcategories() -> Vec<&'static str> {
    CATEGORY_TAXONOMY
        .iter()
        .filter(|(_, _, parent)| parent.is_some())
        .map(|(category_id, _, _)| *category_id)
        .collect()
}

/// One row of this tier, holding the text it derived so [`CatalogRow`] can borrow it.
struct BuiltRow {
    index: usize,
    slug: String,
    name: String,
    short_description: String,
    categories: Vec<&'static str>,
    /// The fulfillment option types this row's record offers, read off the kind of product
    /// it is ([`fulfillment_for_kind`]): one per route that kind is delivered through, so a
    /// row of a multi-route kind offers several options and one of a single-route kind
    /// offers one.
    fulfillment: Vec<&'static str>,
    /// The key features its record offers customers, read off the kind of product it is
    /// ([`highlights_for_kind`]).
    highlights: Vec<&'static str>,
    /// How its offer is priced and what that pricing is metered in, read off the kind of
    /// product it is ([`pricing_for_kind`]).
    pricing: (&'static str, &'static str),
    support_phone: String,
}

impl BuiltRow {
    /// Derive a row from the subcategory it belongs to and the one it is also classified
    /// under: the category's own display name is what the listing is named and described
    /// by, so a `searchText` keyword or a `CATEGORY` facet value reaches it by the same
    /// word a client reads.
    fn new(index: usize, category: &'static str, paired: &'static str) -> Self {
        let kind = PRODUCT_KINDS[index % PRODUCT_KINDS.len()];
        let brand = brand(index);
        let display_name = taxonomy_category(category).display_name;
        let paired_name = taxonomy_category(paired).display_name;
        Self {
            index,
            slug: format!("{}-{}", category, kind.to_lowercase()),
            name: format!("{brand} {display_name} {kind}"),
            short_description: format!(
                "{display_name} {} with automation, monitoring and {} integration.",
                kind.to_lowercase(),
                paired_name.to_lowercase()
            ),
            categories: vec![category, paired],
            fulfillment: fulfillment_for_kind(kind).to_vec(),
            highlights: highlights_for_kind(kind).to_vec(),
            pricing: pricing_for_kind(kind),
            support_phone: format!(
                "+1-{}-555-{:04}",
                PHONE_AREA_CODES[(index / PHONE_LINES) % PHONE_AREA_CODES.len()],
                PHONE_BLOCK + index % PHONE_LINES
            ),
        }
    }

    /// This row as the seeder's own description of a record block.
    fn row(&self) -> CatalogRow<'_> {
        let index = self.index;
        CatalogRow {
            sequence: SEQUENCE_BLOCK + index,
            slug: &self.slug,
            name: &self.name,
            short_description: &self.short_description,
            categories: &self.categories,
            // The options the record offers, which this tier reads off the kind of product
            // the row is rather than from the row's position in the model's enum.
            fulfillment: &self.fulfillment,
            highlights: &self.highlights,
            seller: BREADTH_SELLERS[index % BREADTH_SELLERS.len()],
            // The pricing the record is sold under, read off the kind of product the row is
            // for the same reason its fulfillment is: both are properties of the record, not
            // of where it happens to sit in this tier.
            pricing_model: self.pricing.0,
            pricing_unit: self.pricing.1,
            reviews: BREADTH_REVIEWS[(index + 2) % BREADTH_REVIEWS.len()],
            support_phone: &self.support_phone,
            // Every sixth row's offer belongs to an offer set, so this tier reaches
            // `OFFERSET` purchase options across the catalog as well.
            offer_set: index % 6 == 5,
        }
    }
}

/// The brand a row's name starts with: the first word of the seller that publishes it, so
/// the listing name and its publisher read as the same vendor's.
fn brand(index: usize) -> &'static str {
    let (_, seller_name) = BREADTH_SELLERS[index % BREADTH_SELLERS.len()];
    seller_name
        .split_whitespace()
        .next()
        .unwrap_or(seller_name)
}
