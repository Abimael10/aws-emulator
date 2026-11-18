// SPDX-License-Identifier: CC0-1.0

//! The managed PRODUCTS this service publishes: both vendor families of managed
//! rule group — the Amazon Web Services Managed Rules rule sets this service
//! catalogues ([`MANAGED_RULE_GROUP_CATALOGUE`]) and the Amazon Web Services
//! Marketplace sellers' listings — and what one of them is reported as.
//!
//! `ManagedProductDescriptor` is "the properties of a managed product, such as an
//! Amazon Web Services Managed Rules rule group OR an Amazon Web Services
//! Marketplace managed rule group" (the shape's own doc), and
//! `DescribeAllManagedProducts` "provides high-level information for the Amazon Web
//! Services Managed Rules rule groups AND Amazon Web Services Marketplace managed
//! rule groups" (the operation's), so the product catalogue is the UNION of the two
//! families rather than either one alone.
//!
//! The two families differ in what a descriptor carries, which is why a seller's
//! listing is held here rather than as another [`ManagedRuleGroupDescriptor`]:
//!
//! * `ProductLink` is "for Amazon Web Services Marketplace managed rule groups
//!   only, the link to the rule group product page", so only a seller's listing
//!   carries one.
//! * `SnsTopicArn` is the topic "used to provide notification of changes to the
//!   managed rule group", and the vendor that publishes the group owns that topic —
//!   one per VENDOR, so each seller has its own and not the Amazon Web Services
//!   Managed Rules one.
//! * A seller's listing is not something this account may RUN:
//!   `ListAvailableManagedRuleGroups` lists "all Amazon Web Services Managed Rules
//!   rule groups and all of the Amazon Web Services Marketplace managed rule groups
//!   that you're SUBSCRIBED to" (its own doc), and nothing here subscribes this
//!   account to a seller — so a listing is reported as a product, is not listed as
//!   available, and a rule that runs one is refused as unsubscribed
//!   ([`unsubscribed_managed_rule_group`]).

use super::*;

/// The address an Amazon Web Services Marketplace listing's product page is
/// published under, without its listing segment.
///
/// `ManagedProductDescriptor$ProductLink` is "the link to the rule group product
/// page" of a Marketplace managed rule group, and a Marketplace listing's product
/// page lives under this address followed by the identifier the listing is
/// published as (see [`MarketplaceManagedProduct::product_link`]).
pub const MARKETPLACE_PRODUCT_PAGE_ADDRESS: &str = "https://aws.amazon.com/marketplace/pp";

/// An Amazon Web Services Marketplace SELLER of managed rule groups: the name its
/// groups are published under and the topic it announces changes to them on.
///
/// A vendor is identified by its name (`VendorName` is "the name of the managed
/// rule group vendor. You use this, along with the rule group name, to identify a
/// rule group") and announces changes to everything it publishes on ONE topic, the
/// way Amazon Web Services Managed Rules do
/// ([`AWS_MANAGED_RULE_NOTIFICATION_TOPIC`]) — so the pair sits beside the seller
/// rather than being repeated on each of its listings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketplaceSeller {
    /// The seller's own name, as its listings publish it (`VendorName`:
    /// `length 1..128`, pattern `\S`).
    pub vendor_name: &'static str,
    /// The name of the topic this seller announces changes to its managed rule
    /// groups on, which `SnsTopicArn` reports the ARN of.
    ///
    /// A seller's topic lives in the seller's OWN account, which is not something
    /// this emulator holds: every ARN this service answers is minted through the
    /// emulator's own ARN builder, so what distinguishes one vendor's topic from
    /// another's here is the topic name, published per seller.
    pub notification_topic: &'static str,
}

/// One Amazon Web Services Marketplace managed rule group, as its seller lists it.
///
/// Flat and `Copy` like [`ManagedRuleGroupDescriptor`], and for the same reason: a
/// listing is published data rather than state this account owns — nothing creates,
/// updates or deletes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketplaceManagedProduct {
    /// The seller that publishes this listing.
    pub seller: &'static MarketplaceSeller,
    /// The name of the managed rule group the listing offers
    /// (`ManagedProductDescriptor$ManagedRuleSetName`, an `EntityName`:
    /// `length 1..128`, pattern `^[\w\-]+$`), in the seller's own naming form.
    pub managed_rule_set_name: &'static str,
    /// The listing's display name (`ProductTitle` is "the display name for the
    /// managed rule group"), where this catalogue can cite the seller's published
    /// title; `None` where it cannot.
    ///
    /// Cited or omitted, never composed: a seller's listing text is the seller's, so
    /// a listing whose title this catalogue cannot cite reports none rather than a
    /// text nobody published — the same reading
    /// [`PublishedManagedRule::published_label`] takes of a label.
    pub product_title: Option<&'static str>,
    /// The listing's one-line summary (`ProductDescription` is "a short description
    /// of the managed rule group"), in the seller's published words where this
    /// catalogue can cite them, and `None` where it cannot (see
    /// [`Self::product_title`]).
    pub product_description: Option<&'static str>,
    /// The segment the seller's product page is published under on Amazon Web
    /// Services Marketplace: the listing code the page is addressed by, which
    /// [`MARKETPLACE_PRODUCT_PAGE_ADDRESS`] is followed by to form `ProductLink`.
    ///
    /// Published per listing rather than derived from the group's name or its
    /// identifier, because a listing code is the Marketplace's own and reads nothing
    /// like either.
    pub product_page: &'static str,
    /// Whether the seller offers this group in named versions a web ACL can choose
    /// between (`IsVersioningSupported`), reported as stored like the same property
    /// of an Amazon Web Services Managed Rules group
    /// ([`ManagedRuleGroupDescriptor::is_versioning_supported`]).
    pub is_versioning_supported: bool,
}

impl MarketplaceManagedProduct {
    /// This listing as the managed PRODUCT a describe reports it.
    ///
    /// `IsAdvancedManagedRuleSet` is "whether the rule group provides an advanced
    /// set of protections, such as the Amazon Web Services Managed Rules rule groups
    /// that are used for WAF intelligent threat mitigation" (the member's own doc):
    /// those groups are the advanced ones ([`advanced`]), so a seller's listing
    /// reports `false`.
    pub fn product(&self) -> ManagedProductDescriptor {
        ManagedProductDescriptor {
            vendor_name: Some(self.seller.vendor_name.to_string()),
            managed_rule_set_name: Some(self.managed_rule_set_name.to_string()),
            product_id: Some(managed_product_id(
                self.seller.vendor_name,
                self.managed_rule_set_name,
            )),
            product_link: Some(self.product_link()),
            product_title: self.product_title.map(str::to_string),
            product_description: self.product_description.map(str::to_string),
            sns_topic_arn: Some(managed_product_topic_arn(self.seller.notification_topic)),
            is_versioning_supported: self.is_versioning_supported,
            is_advanced_managed_rule_set: false,
        }
    }

    /// The link to this listing's product page: the Marketplace product-page
    /// address ([`MARKETPLACE_PRODUCT_PAGE_ADDRESS`]) followed by the listing code
    /// the page is published under ([`Self::product_page`]).
    ///
    /// `ProductLink` is "for Amazon Web Services Marketplace managed rule groups
    /// only, the link to the rule group product page", so it is answered for a
    /// seller's listing and for nothing else — an Amazon Web Services Managed Rules
    /// rule set carries none ([`ManagedRuleGroupDescriptor::product`]).
    pub fn product_link(&self) -> String {
        format!("{MARKETPLACE_PRODUCT_PAGE_ADDRESS}/{}", self.product_page)
    }
}

// ---------------------------------------------------------------------------
// The Marketplace sellers, one constant each, and their listings below
// ([`MARKETPLACE_MANAGED_PRODUCT_CATALOGUE`]). A seller publishes the same
// listings whatever `Scope` a caller asks in, so the catalogue is one list rather
// than one per namespace.
// ---------------------------------------------------------------------------

/// The seller of the `CyberSecurityCloud-` managed rule groups, with the topic it
/// announces changes to them on.
const CYBER_SECURITY_CLOUD: MarketplaceSeller = MarketplaceSeller {
    vendor_name: "Cyber Security Cloud Inc.",
    notification_topic: "CyberSecurityCloud-ManagedRule",
};

/// The Amazon Web Services Marketplace managed rule groups this catalogue
/// publishes, grouped by seller.
///
/// The second family `DescribeAllManagedProducts` reports: third-party sellers'
/// listings in the Marketplace's web application firewall category, each offering a
/// managed rule group a subscriber may reference by the seller's
/// `(VendorName, Name)` pair.
///
/// A seller's inventory is the SELLER's publication, so every entry here is CITED
/// rather than composed: the vendor name, the group name, the listing code its
/// product page is addressed by, the seller's notification topic and whatever of the
/// listing's own text is citable. A listing this catalogue cannot cite is absent
/// rather than invented, and a text it cannot cite is omitted rather than worded here
/// ([`MarketplaceManagedProduct::product_title`]) — adding a seller's listing is a
/// data change in this one list. What the catalogue publishes per entry is the
/// product's identifier and the ARN of its seller's topic ([`managed_product_id`],
/// [`managed_product_topic_arn`]), because a seller's own identifiers and account are
/// not observable here.
pub const MARKETPLACE_MANAGED_PRODUCT_CATALOGUE: &[MarketplaceManagedProduct] = &[
    listing(
        &CYBER_SECURITY_CLOUD,
        "CyberSecurityCloud-HighSecurityOWASPSet-",
        None,
        None,
        "B081TD6XL1",
    ),
    listing(
        &CYBER_SECURITY_CLOUD,
        "CyberSecurityCloud-APIGatewayServerless-",
        Some("Cyber Security Cloud Managed Rules for AWS WAF -API Gateway/Serverless-"),
        Some(
            "The Cyber Security Cloud OWASP ruleset is designed to mitigate and \
             minimize vulnerabilities, including all those on OWASP API \
             Security/Serverless Top 10 Threats.",
        ),
        "B081TKTBQ8",
    ),
];

/// One [`MARKETPLACE_MANAGED_PRODUCT_CATALOGUE`] entry: a seller's listing of a
/// managed rule group it offers in named versions.
///
/// A Marketplace seller publishes its rule group as a versioned managed rule set —
/// "vendors, you can use the managed rule set APIs to provide controlled rollout of
/// your VERSIONED managed rule group offerings for your customers"
/// (`ManagedRuleSetSummary`'s own doc, whose vendors "are Amazon Web Services and
/// Amazon Web Services Marketplace sellers") — so a listing built here reports its
/// versioning as supported, which is what tells a subscriber it can pin a version of
/// it.
const fn listing(
    seller: &'static MarketplaceSeller,
    managed_rule_set_name: &'static str,
    product_title: Option<&'static str>,
    product_description: Option<&'static str>,
    product_page: &'static str,
) -> MarketplaceManagedProduct {
    MarketplaceManagedProduct {
        seller,
        managed_rule_set_name,
        product_title,
        product_description,
        product_page,
        is_versioning_supported: true,
    }
}

/// Every managed product this service publishes, in published order: the Amazon Web
/// Services Marketplace sellers' listings, then the Amazon Web Services Managed
/// Rules rule sets.
///
/// `DescribeAllManagedProducts` reports "the Amazon Web Services Managed Rules rule
/// groups and Amazon Web Services Marketplace managed rule groups" (its own doc), so
/// the report is the union of both families — one descriptor per product of either —
/// and a product of one family is reported exactly as that family publishes it (see
/// [`MarketplaceManagedProduct::product`] and
/// [`ManagedRuleGroupDescriptor::product`]).
///
/// The same products whatever `Scope` the request names: a product is a vendor's
/// publication, and a scope is only the namespace a request is made in.
pub fn managed_products() -> Vec<ManagedProductDescriptor> {
    MARKETPLACE_MANAGED_PRODUCT_CATALOGUE
        .iter()
        .map(MarketplaceManagedProduct::product)
        .chain(
            MANAGED_RULE_GROUP_CATALOGUE
                .iter()
                .map(ManagedRuleGroupDescriptor::product),
        )
        .collect()
}

/// The managed products `vendor_name` publishes, in the same order and with the same
/// descriptors [`managed_products`] reports them in.
///
/// `DescribeManagedProductsByVendor` "provides high-level information for the managed
/// rule groups owned by a specific vendor" (its own doc), and `VendorName` is matched
/// exactly, like the vendor segment of the pair that identifies a group: a vendor
/// that publishes nothing here is reported an empty list rather than a refusal.
pub fn managed_products_of_vendor(vendor_name: &str) -> Vec<ManagedProductDescriptor> {
    managed_products()
        .into_iter()
        .filter(|product| product.vendor_name.as_deref() == Some(vendor_name))
        .collect()
}

/// The identifier the managed product of `vendor_name`'s group `managed_rule_set_name`
/// is published as: stable per product, distinct per product, and not the group's
/// name.
///
/// `ManagedProductDescriptor$ProductId` is "a unique identifier for the rule group.
/// This ID is returned in the responses to create and list commands. You provide it
/// to operations like update and delete" — the same thing `ManagedRuleSetSummary$Id`
/// is for the vendor that maintains the set, which is an `EntityId`: a UUID
/// (`^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$`). So a product's identifier is
/// its own, separate from the `ManagedRuleSetName` a caller writes in a
/// `ManagedRuleGroupStatement`, and a caller that round-trips one into another call
/// keys on this.
///
/// A vendor assigns that identifier when it publishes the set, which is not something
/// this emulator observes, so the catalogue publishes one: the UUID the identifying
/// pair spells, shaped like every other id this service mints ([`new_uuid`]) but
/// DERIVED from the pair rather than from fresh entropy — so the same product is
/// reported under the same identifier on every call and by every operation that
/// reports it, and no two products share one.
pub fn managed_product_id(vendor_name: &str, managed_rule_set_name: &str) -> String {
    use ::sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    // The pair, each part length-prefixed, so no two different pairs hash the same
    // bytes.
    for part in [vendor_name, managed_rule_set_name] {
        hasher.update(part.len().to_le_bytes());
        hasher.update(part.as_bytes());
    }
    let digest: [u8; 32] = hasher.finalize().into();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid_text(bytes)
}

/// The ARN of the topic a vendor announces changes to its managed rule groups on,
/// from the topic's name.
///
/// `ManagedProductDescriptor$SnsTopicArn` is the ARN of the topic "that's used to
/// provide notification of changes to the managed rule group. You can subscribe to
/// the SNS topic to receive notifications when the managed rule group is modified",
/// and the vendor that publishes the group owns that topic — so the ARN is per
/// VENDOR, the same for every group one vendor publishes and different for every
/// vendor. Minted through the emulator's own ARN builder, like every other ARN this
/// service answers, in the region a notification topic is published in
/// ([`AWS_MANAGED_RULE_NOTIFICATION_REGION`]).
pub fn managed_product_topic_arn(topic: &str) -> String {
    ::engine::emulator_arn("sns", AWS_MANAGED_RULE_NOTIFICATION_REGION, topic)
}

/// The Marketplace listing the pair `(vendor_name, name)` names, if a seller in
/// this catalogue offers that managed rule group.
///
/// Matched exactly on both members, like [`managed_rule_group`]: a managed rule
/// group is identified by its `(VendorName, Name)` pair and by nothing else.
pub fn marketplace_managed_product(
    vendor_name: &str,
    name: &str,
) -> Option<&'static MarketplaceManagedProduct> {
    MARKETPLACE_MANAGED_PRODUCT_CATALOGUE
        .iter()
        .find(|listing| {
            listing.seller.vendor_name == vendor_name && listing.managed_rule_set_name == name
        })
}

/// The sentence `WAFSubscriptionNotFoundException` carries: the shape's own
/// documented sentence
/// (`smithy_query(query="shape",shape="WAFSubscriptionNotFoundException")`, its
/// wrapping whitespace flattened), which names no particular group — what a caller
/// has to correct is its subscription, not an input.
pub const SUBSCRIPTION_NOT_FOUND_MESSAGE: &str =
    "You tried to use a managed rule group that's available by subscription, but \
     you aren't subscribed to it yet.";

/// `WAFSubscriptionNotFoundException` as WAF returns it: nothing but
/// [`SUBSCRIPTION_NOT_FOUND_MESSAGE`].
pub fn subscription_not_found_exception() -> WAFSubscriptionNotFoundException {
    WAFSubscriptionNotFoundException {
        message: Some(SUBSCRIPTION_NOT_FOUND_MESSAGE.to_string()),
    }
}

/// The refusal a rule list earns by RUNNING a managed rule group that is available
/// by SUBSCRIPTION and that this account is not subscribed to, if one of its rules
/// runs one.
///
/// A Marketplace seller's group is published — a managed-product describe reports it
/// ([`managed_products`]) — but using it takes a subscription to that seller's
/// listing: `ListAvailableManagedRuleGroups` lists "all Amazon Web Services Managed
/// Rules rule groups and all of the Amazon Web Services Marketplace managed rule
/// groups that you're SUBSCRIBED to" (its own doc), and the model gives the save
/// path its own `WAFSubscriptionNotFoundException` — "you tried to use a managed
/// rule group that's available by subscription, but you aren't subscribed to it
/// yet" — for exactly this case. Nothing in this API subscribes an account to a
/// seller, so every listing here is unsubscribed: a rule that runs one is refused
/// as unsubscribed rather than as a group nobody publishes, which is what tells a
/// caller to subscribe rather than to correct the pair. A pair NO vendor publishes
/// stays [`ComAmazonawsWafv2Awswaf20190729State::unresolvable_rule_reference`]'s
/// `WAFNonexistentItemException`.
///
/// Walked with an explicit worklist, like
/// [`expired_managed_rule_group_version`]: the tree is supplied by the request.
pub fn unsubscribed_managed_rule_group(
    rules: &[Rule],
) -> Option<WAFSubscriptionNotFoundException> {
    let mut pending: Vec<&Statement> = rules.iter().map(|rule| &rule.statement).collect();
    while let Some(statement) = pending.pop() {
        if statement
            .managed_rule_group_statement
            .as_ref()
            .is_some_and(|managed| {
                marketplace_managed_product(&managed.vendor_name, &managed.name).is_some()
            })
        {
            return Some(subscription_not_found_exception());
        }
        pending.extend(nested_statements(statement));
    }
    None
}
