// SPDX-License-Identifier: CC0-1.0

//! Which of the patches Patch Manager publishes are AVAILABLE to be put in a patch
//! baseline, and how the filters of that read select them.
//!
//! [`super::patch_catalog`] holds the published catalogue itself — one patch per published
//! product per monthly release — and pairs it with the approval state one baseline gives
//! each patch. This module is the OTHER read of the same catalogue: "lists all patches
//! eligible to be included in a patch baseline" (`DescribeAvailablePatches`), which names
//! no baseline and no operating system and instead narrows the catalogue by the patch
//! properties its `Filters` search on.
//!
//! Two rules decide which patches such a read walks:
//!
//! * The operation reports the patches of the operating systems it supports —
//!   "currently, `DescribeAvailablePatches` supports only the Amazon Linux 1, Amazon Linux
//!   2, and Windows Server operating systems" ([`AVAILABLE_PATCH_OPERATING_SYSTEMS`]).
//! * Which of those operating systems a request is about is said by its `PRODUCT` key:
//!   "when specifying filters for Linux patches, you must specify a key-pair for
//!   `PRODUCT`" (`DescribeAvailablePatches$Filters`), so a request naming products walks
//!   the catalogues that publish them, and one naming none walks the Windows catalogue —
//!   the one set of patches that can be asked for without naming a product.
//!
//! The filter keys are the two lists that doc publishes ([`AVAILABLE_PATCH_FILTER_KEYS`]),
//! each selecting on a patch property the catalogue holds ([`super::patch_catalog`]), so
//! what a filtered read reports is a projection of the one published catalogue and no call
//! of this service's model adds a patch to it.

use std::time::SystemTime;

use super::patch_catalog::{
    published_patches, PublishedPatch, FILTER_KEY_ADVISORY_ID, FILTER_KEY_ARCH,
    FILTER_KEY_BUGZILLA_ID, FILTER_KEY_CVE_ID, FILTER_KEY_EPOCH, FILTER_KEY_NAME,
    FILTER_KEY_PATCH_ID, FILTER_KEY_PATCH_SET, FILTER_KEY_RELEASE, FILTER_KEY_REPOSITORY,
    FILTER_KEY_VERSION, FILTER_VALUE_WILDCARD,
};
use super::patch_properties::{
    PATCH_SET_OPERATING_SYSTEM, PROPERTY_CLASSIFICATION, PROPERTY_MSRC_SEVERITY, PROPERTY_PRODUCT,
    PROPERTY_PRODUCT_FAMILY, PROPERTY_SEVERITY, PUBLISHED_PATCH_METADATA,
};
use crate::ops::types::PatchOrchestratorFilter;

/// The operating systems whose published patches an available-patch read reports:
/// "currently, `DescribeAvailablePatches` supports only the Amazon Linux 1, Amazon Linux 2,
/// and Windows Server operating systems" (`DescribeAvailablePatches`), each named as the
/// modelled `OperatingSystem` value it is (`crate::ops::types::OPERATING_SYSTEM_VALUES`).
pub const AVAILABLE_PATCH_OPERATING_SYSTEMS: &[&str] = &[
    "AMAZON_LINUX",
    "AMAZON_LINUX_2",
    PATCH_SET_OPERATING_SYSTEM,
];

/// The keys an available-patch read searches Windows patches on: "supported keys for
/// Windows Server managed node patches include the following: `PATCH_SET`, `PRODUCT`,
/// `PRODUCT_FAMILY`, `MSRC_SEVERITY`, `CLASSIFICATION`, `PATCH_ID`" (same doc), in the
/// order it lists them.
///
/// These are also the keys a request may search on WITHOUT naming a product, because they
/// are the ones Windows patches carry ([`addressed_operating_systems`]).
pub const WINDOWS_AVAILABLE_PATCH_FILTER_KEYS: &[&str] = &[
    FILTER_KEY_PATCH_SET,
    PROPERTY_PRODUCT,
    PROPERTY_PRODUCT_FAMILY,
    PROPERTY_MSRC_SEVERITY,
    PROPERTY_CLASSIFICATION,
    FILTER_KEY_PATCH_ID,
];

/// The keys the same read searches Linux patches on: "supported keys for Linux managed node
/// patches include the following: `PRODUCT`, `NAME`, `SEVERITY`, `EPOCH`, `VERSION`,
/// `RELEASE`, `ARCH`, `REPOSITORY`, `ADVISORY_ID`, `CVE_ID`, `BUGZILLA_ID`" (same doc), in
/// the order it lists them.
pub const LINUX_AVAILABLE_PATCH_FILTER_KEYS: &[&str] = &[
    PROPERTY_PRODUCT,
    FILTER_KEY_NAME,
    PROPERTY_SEVERITY,
    FILTER_KEY_EPOCH,
    FILTER_KEY_VERSION,
    FILTER_KEY_RELEASE,
    FILTER_KEY_ARCH,
    FILTER_KEY_REPOSITORY,
    FILTER_KEY_ADVISORY_ID,
    FILTER_KEY_CVE_ID,
    FILTER_KEY_BUGZILLA_ID,
];

/// Every key an available-patch read searches on: the Windows keys, then the Linux keys the
/// Windows list does not already hold. The `PatchOrchestratorFilter$Key` member is a plain
/// string in the model, so the vocabulary is enforced here, and this is also what a refusal
/// names back to the caller.
pub const AVAILABLE_PATCH_FILTER_KEYS: &[&str] = &[
    FILTER_KEY_PATCH_SET,
    PROPERTY_PRODUCT,
    PROPERTY_PRODUCT_FAMILY,
    PROPERTY_MSRC_SEVERITY,
    PROPERTY_CLASSIFICATION,
    FILTER_KEY_PATCH_ID,
    FILTER_KEY_NAME,
    PROPERTY_SEVERITY,
    FILTER_KEY_EPOCH,
    FILTER_KEY_VERSION,
    FILTER_KEY_RELEASE,
    FILTER_KEY_ARCH,
    FILTER_KEY_REPOSITORY,
    FILTER_KEY_ADVISORY_ID,
    FILTER_KEY_CVE_ID,
    FILTER_KEY_BUGZILLA_ID,
];

/// What a `Filters` entry naming no filter key at all is refused with. Both members of a
/// `PatchOrchestratorFilter` are optional in the model, so such an entry reaches the
/// handler, and it names no patch property for the read to be narrowed by. The sentence is
/// the one this service words that class of mistake with, shared rather than re-worded.
pub const AVAILABLE_PATCH_FILTER_KEY_MISSING: &str =
    super::patch_baselines::PATCH_BASELINE_FILTER_KEY_MISSING;

/// What a `Filters` entry naming a key but no value is refused with, worded as the sibling
/// patch reads of this service word it.
pub fn available_patch_filter_value_missing(key: &str) -> String {
    super::patch_baselines::patch_baseline_filter_value_missing(key)
}

/// What a request searching on a key only Linux patches carry, without saying WHICH Linux
/// patches, is refused with: the rule the member's own doc states — "when specifying
/// filters for Linux patches, you must specify a key-pair for `PRODUCT`"
/// (`DescribeAvailablePatchesRequest$Filters`), which that doc illustrates with a command
/// that fails for naming `CVE_ID` alone and the same command succeeding once it names a
/// product too.
pub const LINUX_FILTER_REQUIRES_PRODUCT: &str =
    "When specifying filters for Linux patches, you must specify a key-pair for PRODUCT";

/// Whether `key` is a key an available-patch read searches on
/// ([`AVAILABLE_PATCH_FILTER_KEYS`]).
pub fn is_available_patch_filter_key(key: &str) -> bool {
    AVAILABLE_PATCH_FILTER_KEYS.contains(&key)
}

/// Whether `key` searches on a property only Linux patches carry, which is every accepted
/// key outside the Windows list ([`WINDOWS_AVAILABLE_PATCH_FILTER_KEYS`]).
pub fn is_linux_only_filter_key(key: &str) -> bool {
    !WINDOWS_AVAILABLE_PATCH_FILTER_KEYS.contains(&key)
}

/// The lower bound the modelled page size of an available-patch read carries
/// (`DescribeAvailablePatchesRequest$MaxResults`, range 1..100).
pub const MIN_AVAILABLE_PATCH_PAGE_SIZE: i32 = 1;

/// The upper bound of the same member, and the page a request naming no `MaxResults` is
/// served up to: "the maximum number of patches to return (per page)" bounds a page the
/// caller asked to be bounded, so a caller asking for no bound is served the whole page the
/// operation allows.
pub const MAX_AVAILABLE_PATCH_PAGE_SIZE: i32 = 100;

/// The page size an available-patch read serves, or the refusal message for a `MaxResults`
/// outside the modelled range — which the boundary walk already refuses, so this is a
/// backstop rather than the first line of defence (see [`super::page_size`]).
pub fn available_patch_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(
        max_results,
        MIN_AVAILABLE_PATCH_PAGE_SIZE,
        MAX_AVAILABLE_PATCH_PAGE_SIZE,
    )
}

/// One filter of an available-patch read, read off the request: the patch property it
/// searches on and the values it accepts for it.
///
/// Borrowed from the request rather than copied out of it: a filter is read, matched and
/// dropped within the one call that carries it.
#[derive(Debug, Clone, Copy)]
pub struct AvailablePatchFilter<'a> {
    /// The key this filter searches on, one of [`AVAILABLE_PATCH_FILTER_KEYS`].
    pub key: &'a str,
    /// "The value for the filter" (`PatchOrchestratorFilter$Values`): the values that key
    /// accepts, any one of which selects a patch.
    pub values: &'a [String],
}

/// The filters of an available-patch read, or the sentence the request is refused with.
///
/// A filter narrows the read by a property of a patch, so it has to name the key it
/// searches on and a value to search for, and the key has to be one this read's own
/// vocabulary holds — the filter shape is shared by several patch reads, each accepting its
/// own keys, so a key outside it is refused rather than silently matching nothing. A key
/// only Linux patches carry says nothing about WHICH Linux patches on its own, which is the
/// combination the member's doc states cannot hold ([`LINUX_FILTER_REQUIRES_PRODUCT`]).
pub fn available_patch_filters(
    filters: &[PatchOrchestratorFilter],
) -> Result<Vec<AvailablePatchFilter<'_>>, String> {
    let mut read = Vec::new();
    for filter in filters {
        let key = filter.key.as_deref().unwrap_or_default();
        if key.is_empty() {
            return Err(AVAILABLE_PATCH_FILTER_KEY_MISSING.to_string());
        }
        if !is_available_patch_filter_key(key) {
            return Err(super::invalid_filter_key_message(
                key,
                AVAILABLE_PATCH_FILTER_KEYS,
            ));
        }
        let values = filter.values.as_deref().unwrap_or_default();
        if values.is_empty() {
            return Err(available_patch_filter_value_missing(key));
        }
        read.push(AvailablePatchFilter { key, values });
    }
    if read.iter().any(|filter| is_linux_only_filter_key(filter.key))
        && !read.iter().any(|filter| filter.key == PROPERTY_PRODUCT)
    {
        return Err(LINUX_FILTER_REQUIRES_PRODUCT.to_string());
    }
    Ok(read)
}

/// The operating systems whose published catalogue a read carrying `filters` walks.
///
/// The `PRODUCT` keys of the request say it: an operating system is addressed when it
/// publishes one of the products searched for, or when a value is the wildcard that
/// "matches all values" (`PatchFilter`). A request naming no product at all walks the
/// Windows catalogue, which is the one set of patches that can be asked for without naming
/// a product — "when specifying filters for Linux patches, you must specify a key-pair for
/// `PRODUCT`".
pub fn addressed_operating_systems(filters: &[AvailablePatchFilter<'_>]) -> Vec<&'static str> {
    let products: Vec<&str> = filters
        .iter()
        .filter(|filter| filter.key == PROPERTY_PRODUCT)
        .flat_map(|filter| filter.values.iter().map(String::as_str))
        .collect();
    if products.is_empty() {
        return vec![PATCH_SET_OPERATING_SYSTEM];
    }
    AVAILABLE_PATCH_OPERATING_SYSTEMS
        .iter()
        .copied()
        .filter(|operating_system| {
            products.iter().any(|product| {
                *product == FILTER_VALUE_WILDCARD || publishes_product(operating_system, product)
            })
        })
        .collect()
}

/// Whether the patch metadata published for `operating_system` holds `product` among its
/// products ([`PUBLISHED_PATCH_METADATA`]), which is what makes a `PRODUCT` value name that
/// operating system's catalogue.
fn publishes_product(operating_system: &str, product: &str) -> bool {
    PUBLISHED_PATCH_METADATA
        .iter()
        .filter(|group| group.operating_system == operating_system)
        .any(|group| group.products.contains(&product))
}

/// The patches an available-patch read carrying `filters` reports, newest release first.
///
/// The catalogues of the addressed operating systems ([`addressed_operating_systems`]),
/// narrowed to the patches meeting EVERY filter the request carries, in a total order taken
/// from the patches themselves — so two reads report the same sequence and a page resumes
/// at the patch the last one stopped at. A filter searching on a property the addressed
/// patches carry no value for selects none of them, which is what a read of a catalogue
/// that publishes no such property can report.
pub fn available_patches(
    filters: &[AvailablePatchFilter<'_>],
    at: SystemTime,
) -> Vec<PublishedPatch> {
    let mut patches: Vec<PublishedPatch> = addressed_operating_systems(filters)
        .into_iter()
        .flat_map(|operating_system| published_patches(operating_system, at))
        .filter(|patch| {
            filters
                .iter()
                .all(|filter| patch.matches_filter(filter.key, filter.values))
        })
        .collect();
    patches.sort_by(|left, right| {
        right
            .release_date
            .cmp(&left.release_date)
            .then_with(|| left.product.cmp(&right.product))
            .then_with(|| left.id.cmp(&right.id))
    });
    patches
}

impl super::ComAmazonawsSsmAmazonssmState {
    /// The patches this store reports as available under `filters` ([`available_patches`]).
    ///
    /// The one door an available-patch read goes through, the way a patch property resolves
    /// through [`super::ComAmazonawsSsmAmazonssmState::patch_property_values`]: the patches
    /// are Patch Manager's own published catalogue, held by the store rather than by any one
    /// request, so two reads report the same patches and a call changes none of them.
    pub fn available_patches(
        &self,
        filters: &[AvailablePatchFilter<'_>],
        at: SystemTime,
    ) -> Vec<PublishedPatch> {
        available_patches(filters, at)
    }
}
