// SPDX-License-Identifier: CC0-1.0

//! Which `PatchFilter` keys a patch baseline of one operating system may select on.
//!
//! "The key for the filter. Run the `DescribePatchProperties` command to view lists of
//! valid keys for each operating system type" (`PatchFilter$Key`): a filter key is a patch
//! PROPERTY — "a patch filter consists of a key and a set of values. The filter key is a
//! patch property. For example, the available filter keys for `WINDOWS` are `PATCH_SET`,
//! `PRODUCT`, `PRODUCT_FAMILY`, `CLASSIFICATION`, and `MSRC_SEVERITY`" (`PatchFilter`) — so
//! membership in the modelled `PatchFilterKey` enum is not enough on its own: the key has
//! to name a property the patches of the baseline's own operating system carry. A key
//! naming a property of ANOTHER operating system's patches selects no patch of this one at
//! all, so a write that states one is refused rather than persisted.
//!
//! Three published facts say which properties an operating system's patches carry, and this
//! module is the one place they meet:
//!
//! * the per-operating-system property lists `DescribePatchProperties` publishes ("DEBIAN
//!   Valid properties: `PRODUCT` | `PRIORITY`"), which [`super::patch_properties`] holds as
//!   the catalogue those reads project
//!   ([`super::patch_properties::publishes_property`]) — so one table backs the read of a
//!   property and the write of a filter on it;
//! * the two keys only the Windows catalogue carries: the patch set a patch belongs to,
//!   "not applicable for the Linux or macOS operating systems"
//!   (`DescribePatchProperties$PatchSet`), and the Knowledge Base id that names an
//!   individual patch, "applies to Windows patches only" (`Patch$KbNumber`);
//! * the properties only a package of a Linux distribution carries — its name, epoch,
//!   version, release, architecture, section, repository and the advisory, vulnerability
//!   and bug ids it is published through — each of which "applies to Linux-based managed
//!   nodes only" (`Patch$Name`, `Patch$Epoch`, `Patch$Version`, `Patch$Release`,
//!   `Patch$Arch`, `Patch$Repository`, `Patch$AdvisoryIds`, `Patch$CVEIds`,
//!   `Patch$BugzillaIds`).
//!
//! Every write of a baseline is held to this ([`patch_filter_key_rejection`]), over both
//! places a write states filters — the baseline's own `GlobalFilters` and the
//! `PatchFilterGroup` of each of its approval rules — so a baseline whose criteria could
//! never match a patch of its operating system can be neither created nor updated into
//! existence, and the refusal names the key the request handed over.

use super::patch_catalog::{
    FILTER_KEY_ADVISORY_ID, FILTER_KEY_ARCH, FILTER_KEY_BUGZILLA_ID, FILTER_KEY_CVE_ID,
    FILTER_KEY_EPOCH, FILTER_KEY_NAME, FILTER_KEY_PATCH_ID, FILTER_KEY_PATCH_SET,
    FILTER_KEY_RELEASE, FILTER_KEY_REPOSITORY, FILTER_KEY_VERSION,
};
use super::patch_properties::{
    publishes_property, MACOS_OPERATING_SYSTEM, PATCH_SET_OPERATING_SYSTEM,
};
use crate::ops::types::{PatchFilterGroup, PatchRuleGroup};

/// The filter key whose values are the section of the package index a Debian-based
/// distribution publishes a package in. One of the modelled `PatchFilterKey` values
/// (`crate::ops::types::PATCH_FILTER_KEY_VALUES`), and a property of a package rather than
/// of a Windows update or a macOS software update.
pub const FILTER_KEY_SECTION: &str = "SECTION";

/// The filter key whose values say which security stream a Linux package is published
/// through. Another modelled `PatchFilterKey` value, and another property of a package.
pub const FILTER_KEY_SECURITY: &str = "SECURITY";

/// Whether the patches Patch Manager publishes for `operating_system` are packages of a
/// Linux distribution, which is what the properties marked "applies to Linux-based managed
/// nodes only" are properties OF.
///
/// Every value the modelled `OperatingSystem` enum declares names a Linux distribution
/// except the two whose updates are not packages at all: Windows, whose patches are
/// operating-system and application updates, and macOS, whose patches are Apple's software
/// updates.
fn publishes_linux_packages(operating_system: &str) -> bool {
    operating_system != PATCH_SET_OPERATING_SYSTEM && operating_system != MACOS_OPERATING_SYSTEM
}

/// Whether a baseline of `operating_system` may select on the `PatchFilter` key `key` —
/// whether that key names a property the patches of that operating system carry.
///
/// The three families of key are decided by the three published facts this module's own doc
/// states; a key outside the modelled `PatchFilterKey` enum, which the boundary walk refuses
/// before a handler runs, names no property of any operating system's patches.
pub fn is_patch_filter_key(operating_system: &str, key: &str) -> bool {
    match key {
        // The patch set a patch belongs to and the Knowledge Base id that names it: both
        // Windows's alone.
        FILTER_KEY_PATCH_SET | FILTER_KEY_PATCH_ID => {
            operating_system == PATCH_SET_OPERATING_SYSTEM
        }
        // The properties of a package, which only a Linux distribution's patches are.
        FILTER_KEY_NAME
        | FILTER_KEY_EPOCH
        | FILTER_KEY_VERSION
        | FILTER_KEY_RELEASE
        | FILTER_KEY_ARCH
        | FILTER_KEY_SECTION
        | FILTER_KEY_SECURITY
        | FILTER_KEY_REPOSITORY
        | FILTER_KEY_ADVISORY_ID
        | FILTER_KEY_CVE_ID
        | FILTER_KEY_BUGZILLA_ID => publishes_linux_packages(operating_system),
        // Every remaining key is one of the properties an operating system PUBLISHES, so
        // the catalogue `DescribePatchProperties` reports from decides it.
        _ => publishes_property(operating_system, key),
    }
}

/// Why the patch filters a write states cannot stand against `operating_system`, or `None`
/// when every one of them names a property that operating system's patches carry.
///
/// Both places a write of a baseline states filters are walked, in the order the request
/// states them — the baseline's global filters first, then each approval rule's own filter
/// group — so the sentence names the FIRST key the operating system does not support, taken
/// from the request verbatim. The whole write is refused on it: a baseline is created or
/// updated with every criterion it states or with none of them.
pub fn patch_filter_key_rejection(
    operating_system: &str,
    global_filters: Option<&PatchFilterGroup>,
    approval_rules: Option<&PatchRuleGroup>,
) -> Option<String> {
    stated_filter_groups(global_filters, approval_rules)
        .flat_map(|group| group.patch_filters.iter())
        .find(|filter| !is_patch_filter_key(operating_system, &filter.key))
        .map(|filter| super::instance_patch_states::unknown_filter_key(&filter.key))
}

/// Every filter group a write of a baseline states: its global filters, then the filter
/// group of each of its approval rules, in the order the request states them.
fn stated_filter_groups<'a>(
    global_filters: Option<&'a PatchFilterGroup>,
    approval_rules: Option<&'a PatchRuleGroup>,
) -> impl Iterator<Item = &'a PatchFilterGroup> {
    global_filters.into_iter().chain(
        approval_rules
            .into_iter()
            .flat_map(|rules| rules.patch_rules.iter())
            .map(|rule| &rule.patch_filter_group),
    )
}
