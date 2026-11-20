// SPDX-License-Identifier: CC0-1.0

//! The individual patches Patch Manager PUBLISHES for an operating system, and which of
//! them one patch baseline makes EFFECTIVE.
//!
//! [`super::patch_properties`] holds the patch metadata Patch Manager publishes as
//! PROPERTY LISTS — the products, product families, classifications and severities an
//! operating system's available patches carry. The patches THEMSELVES are the same
//! catalogue seen one patch at a time: a vendor publishes its updates on a monthly
//! release cycle, so the catalogue of an operating system is one patch per published
//! product per monthly release ([`published_patches`]), carrying the property values that
//! product's group publishes. It exists in every account and Region without anybody
//! creating it, the way the predefined baselines
//! ([`super::patch_baselines::PREDEFINED_BASELINES`]) and the published documents do, and
//! no call of this service's model adds a patch to it.
//!
//! What makes a published patch EFFECTIVE is a patch baseline: "the current effective
//! patches (the patch and the approval state) for the specified patch baseline"
//! (`DescribeEffectivePatchesForPatchBaseline`). The approval state is computed from what
//! the baseline's own write persisted ([`effective_patches`]) rather than stored beside
//! the patch, so a baseline that is created, and later a baseline that is updated, needs
//! no second copy of the catalogue: the patch's own release date and the rule that selects
//! it are what decide whether it "is currently approved, due to be approved by a rule,
//! explicitly approved, or explicitly rejected and the date the patch was or will be
//! approved" (`EffectivePatch`).

use std::fmt::Write as _;
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use super::maintenance_windows::calendar_instant;
use super::patch_baselines::{PatchBaselineRecord, DEFAULT_APPROVED_PATCHES_COMPLIANCE_LEVEL};
use super::patch_properties::{
    PublishedPatchGroup, PATCH_SET_OPERATING_SYSTEM, PROPERTY_CLASSIFICATION,
    PROPERTY_MSRC_SEVERITY, PROPERTY_PRIORITY, PROPERTY_PRODUCT, PROPERTY_PRODUCT_FAMILY,
    PROPERTY_SEVERITY, PUBLISHED_PATCH_METADATA,
};
use crate::ops::types::{EffectivePatch, Patch, PatchFilterGroup, PatchStatus};

/// The patch is approved in the baseline: a rule selected it and the date that rule
/// approves it on has passed. One of the modelled `PatchDeploymentStatus` values
/// (`crate::ops::types::PATCH_DEPLOYMENT_STATUS_VALUES`).
pub const DEPLOYMENT_STATUS_APPROVED: &str = "APPROVED";

/// The patch is "due to be approved by a rule" (`EffectivePatch`): a rule selected it and
/// the date that rule approves it on has not arrived. Another modelled value.
pub const DEPLOYMENT_STATUS_PENDING_APPROVAL: &str = "PENDING_APPROVAL";

/// The baseline names the patch in its "list of explicitly approved patches"
/// (`CreatePatchBaseline$ApprovedPatches`). Another modelled value.
pub const DEPLOYMENT_STATUS_EXPLICIT_APPROVED: &str = "EXPLICIT_APPROVED";

/// The baseline names the patch in its "list of explicitly rejected patches"
/// (`CreatePatchBaseline$RejectedPatches`). The last modelled value.
pub const DEPLOYMENT_STATUS_EXPLICIT_REJECTED: &str = "EXPLICIT_REJECTED";

/// The filter key whose values are the patch sets a patch belongs to: "the available
/// filter keys for `WINDOWS` are `PATCH_SET`, `PRODUCT`, `PRODUCT_FAMILY`,
/// `CLASSIFICATION`, and `MSRC_SEVERITY`" (`PatchFilter`). One of the modelled
/// `PatchFilterKey` values (`crate::ops::types::PATCH_FILTER_KEY_VALUES`).
pub const FILTER_KEY_PATCH_SET: &str = "PATCH_SET";

/// The filter key whose values name individual patches: "`PATCH_ID` Sample values:
/// `KB123456` | `KB4516046`" (`DescribeAvailablePatches$Filters`), so for a Windows patch
/// the value is its Microsoft Knowledge Base id. Another modelled `PatchFilterKey` value.
pub const FILTER_KEY_PATCH_ID: &str = "PATCH_ID";

/// The filter key whose values name the package a Linux patch updates: "`NAME` Sample
/// values: `kernel-headers` | `samba-python` | `php`"
/// (`smithy_query(query="doc", shape="DescribeAvailablePatchesRequest", member="Filters")`).
pub const FILTER_KEY_NAME: &str = "NAME";

/// The filter key whose values are the epoch of that package: "`EPOCH` Sample values: `0` |
/// `1`" (same doc).
pub const FILTER_KEY_EPOCH: &str = "EPOCH";

/// The filter key whose values are its version: "`VERSION` Sample values: `78.6.1` |
/// `4.10.16`" (same doc).
pub const FILTER_KEY_VERSION: &str = "VERSION";

/// The filter key whose values are its release: "`RELEASE` Sample values: `9.56.amzn1` |
/// `1.amzn2`" (same doc).
pub const FILTER_KEY_RELEASE: &str = "RELEASE";

/// The filter key whose values are the architecture it is built for: "`ARCH` Sample values:
/// `i686` | `x86_64`" (same doc).
pub const FILTER_KEY_ARCH: &str = "ARCH";

/// The filter key whose values are the repository it is published in: "`REPOSITORY` Sample
/// values: `Core` | `Updates`" (same doc).
pub const FILTER_KEY_REPOSITORY: &str = "REPOSITORY";

/// The filter key whose values name the advisory a Linux patch is published through:
/// "`ADVISORY_ID` Sample values: `ALAS-2018-1058` | `ALAS2-2021-1594`" (same doc).
pub const FILTER_KEY_ADVISORY_ID: &str = "ADVISORY_ID";

/// The filter key whose values name a vulnerability that advisory addresses: "`CVE_ID`
/// Sample values: `CVE-2018-3615` | `CVE-2020-1472`" (same doc).
pub const FILTER_KEY_CVE_ID: &str = "CVE_ID";

/// The filter key whose values name a bug it closes: "`BUGZILLA_ID` Sample values:
/// `1463241`" (same doc).
pub const FILTER_KEY_BUGZILLA_ID: &str = "BUGZILLA_ID";

/// The epoch the packages this catalogue publishes carry: "the epoch of the patch"
/// (`Patch$Epoch`) is modelled with a default of `0`, and the catalogue publishes no
/// package whose vendor has had to raise it, so every published package carries that
/// default — which is what an `EPOCH` filter searches against.
pub const PUBLISHED_PACKAGE_EPOCH: &str = "0";

/// The classification a patch published through a SECURITY advisory carries, one of the
/// update kinds an RPM-based distribution publishes
/// ([`super::patch_properties::PUBLISHED_PATCH_METADATA`]). It is what gives a patch an
/// advisory id and the vulnerability and bug ids that advisory addresses: the other update
/// kinds are not published through a security advisory at all.
pub const SECURITY_CLASSIFICATION: &str = "Security";

/// The filter value that "matches all values" of its key (`PatchFilter`: "The filter
/// values can be exact values for the patch property given as a key, or a wildcard (*),
/// which matches all values").
pub const FILTER_VALUE_WILDCARD: &str = "*";

/// The one operating system whose patch baselines an effective-patch read reports:
/// "Applies to patch baselines for Windows only"
/// (`DescribeEffectivePatchesForPatchBaseline`).
pub const EFFECTIVE_PATCH_OPERATING_SYSTEM: &str = PATCH_SET_OPERATING_SYSTEM;

/// What an effective-patch read of a baseline of another operating system reports: the
/// clause the modelled refusal words this case with — "the operation isn't supported for
/// the operating system" (`UnsupportedOperatingSystem`) — naming the operating system the
/// baseline applies to.
pub fn unsupported_operating_system(operating_system: &str) -> String {
    format!("The operation isn't supported for the operating system {operating_system}.")
}

/// The lower bound the modelled page size of an effective-patch read carries
/// (`DescribeEffectivePatchesForPatchBaselineRequest$MaxResults`, range 1..100).
pub const MIN_EFFECTIVE_PATCH_PAGE_SIZE: i32 = 1;

/// The upper bound of the same member, and the page a request naming no `MaxResults` is
/// served up to: "the maximum number of patches to return (per page)" bounds a page the
/// caller asked to be bounded, so a caller asking for no bound is served the whole page
/// the operation allows.
pub const MAX_EFFECTIVE_PATCH_PAGE_SIZE: i32 = 100;

/// The page size an effective-patch read serves, or the refusal message for a `MaxResults`
/// outside the modelled range — which the boundary walk already refuses, so this is a
/// backstop rather than the first line of defence (see [`super::page_size`]).
pub fn effective_patch_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(
        max_results,
        MIN_EFFECTIVE_PATCH_PAGE_SIZE,
        MAX_EFFECTIVE_PATCH_PAGE_SIZE,
    )
}

/// How many monthly releases of an operating system's patches the published catalogue
/// holds, counting back from the most recent one.
///
/// This is the EMULATOR's own catalogue depth rather than a figure Patch Manager
/// publishes: a vendor's catalogue grows for as long as it keeps publishing, and a read of
/// it has to answer in one page sequence, so the catalogue here is the most recent year of
/// releases. A rule's approval window is bounded by the same calendar
/// (`PatchRule$ApproveAfterDays`, range 0..360), so a year of releases is enough for every
/// window a rule can state to land inside the catalogue.
pub const MONTHLY_RELEASES: usize = 12;

/// The day of the month a monthly release lands on, counted in whole weeks from the first
/// of the month: a vendor that publishes monthly publishes on the SECOND occurrence of one
/// weekday, so the release of a month is the second [`RELEASE_WEEKDAY`] in it.
const RELEASE_WEEK: u32 = 2;

/// The weekday a monthly release lands on, as the one release instant the model's own
/// example names does (`PatchRule$ApproveAfterDays`: "If a patch is released at
/// `2025-11-09T18:00:00Z`"), which is a Tuesday.
const RELEASE_WEEKDAY: ::chrono::Weekday = ::chrono::Weekday::Tue;

/// The hour of that day, UTC, a release is published at, from the same example instant
/// (`2025-11-09T18:00:00Z`). "Patch Manager evaluates patch release dates using
/// Coordinated Universal Time (UTC)" (`PatchRule$ApproveAfterDays`), so the catalogue
/// holds its release instants in UTC too.
const RELEASE_HOUR_UTC: u32 = 18;

/// How long a day is, which is the unit `ApproveAfterDays` counts in: "the number of days
/// after the release date of each patch matched by the rule that the patch is marked as
/// approved in the patch baseline".
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// The first patch id the catalogue mints, which is what makes every minted Knowledge Base
/// id a seven-digit one, as the published sample values are
/// (`DescribeAvailablePatches$Filters`: "`PATCH_ID` Sample values: `KB123456` |
/// `KB4516046`").
const KB_NUMBER_BASE: u64 = 5_000_000;

/// The vendor of the patches published for the one operating system whose patches carry
/// Knowledge Base ids and Microsoft Security Response Center severities
/// (`Patch$KbNumber`, `Patch$MsrcSeverity`: "Applies to Windows patches only").
const WINDOWS_VENDOR: &str = "Microsoft";

/// The language of a patch that is not language-specific: "The language of the patch if
/// it's language-specific" (`Patch$Language`) — a cumulative operating-system update is
/// published once for every language.
const LANGUAGE_NEUTRAL: &str = "All";

/// One patch the published catalogue holds.
///
/// Every member is derived from the published metadata of the product it applies to and
/// from the release it belongs to, so the catalogue is the same for every caller and a
/// later read reports the patch a earlier read reported.
#[derive(Debug, Clone)]
pub struct PublishedPatch {
    /// "The ID of the patch" (`Patch$Id`), which "isn't the same as the Microsoft
    /// Knowledge Base ID": the catalogue's own identifier for this patch, derived from
    /// the patch's identity so it is stable and unique.
    pub id: String,
    /// "The Microsoft Knowledge Base ID of the patch. Applies to Windows patches only"
    /// (`Patch$KbNumber`), so `None` for every other operating system.
    pub kb_number: Option<String>,
    /// "The title of the patch" (`Patch$Title`).
    pub title: String,
    /// "The date the patch was released" (`Patch$ReleaseDate`): the instant of the
    /// monthly release this patch belongs to.
    pub release_date: SystemTime,
    /// "The name of the vendor providing the patch" (`Patch$Vendor`), where the published
    /// metadata names one.
    pub vendor: Option<String>,
    /// "The product family the patch is applicable for" (`Patch$ProductFamily`), where
    /// the operating system publishes product families.
    pub product_family: Option<String>,
    /// "The specific product the patch is applicable for" (`Patch$Product`).
    pub product: String,
    /// "The classification of the patch" (`Patch$Classification`), where the operating
    /// system publishes classifications.
    pub classification: Option<String>,
    /// "The severity of the patch, such as `Critical`, `Important`, or `Moderate`.
    /// Applies to Windows patches only" (`Patch$MsrcSeverity`).
    pub msrc_severity: Option<String>,
    /// "The severity level of the patch" (`Patch$Severity`), where the operating system
    /// publishes severity levels.
    pub severity: Option<String>,
    /// The package priority this patch carries, where the operating system publishes
    /// priorities instead of classifications — the property a `PRIORITY` patch filter
    /// selects on ([`super::patch_properties::PROPERTY_PRIORITY`]), which is what the
    /// Debian-based distributions publish. Not a member of the modelled `Patch`, so it is
    /// held for selection rather than reported.
    pub priority: Option<String>,
    /// "The language of the patch if it's language-specific" (`Patch$Language`).
    pub language: Option<String>,
    /// The patch set this patch belongs to, which is what a `PATCH_SET` filter selects
    /// on ([`super::patch_properties::PATCH_SET_OS`] or `PATCH_SET_APPLICATION`).
    pub patch_set: &'static str,
    /// "The name of the patch. Applies to Linux-based managed nodes only" (`Patch$Name`):
    /// the package this patch updates, where the operating system's published metadata
    /// holds packages at all, and `None` for a patch that is no package (a Windows update,
    /// a macOS software update).
    pub name: Option<String>,
    /// "The version number of the patch" (`Patch$Version`), for a patch that is a package.
    pub version: Option<String>,
    /// "The particular release of a patch" (`Patch$Release`), for the same.
    pub release: Option<String>,
    /// "The architecture of the patch" (`Patch$Arch`), for the same.
    pub arch: Option<String>,
    /// "The source patch repository for the operating system and version"
    /// (`Patch$Repository`), for the same.
    pub repository: Option<String>,
    /// "The Advisory ID of the patch" (`Patch$AdvisoryIds`): the advisory this patch is
    /// published through, which the distribution publishes for its security fixes
    /// ([`SECURITY_CLASSIFICATION`]), so empty for every other patch.
    pub advisory_ids: Vec<String>,
    /// "The Common Vulnerabilities and Exposures (CVE) ID of the patch" (`Patch$CVEIds`):
    /// the vulnerabilities that advisory addresses.
    pub cve_ids: Vec<String>,
    /// "The Bugzilla ID of the patch" (`Patch$BugzillaIds`): the bugs it closes.
    pub bugzilla_ids: Vec<String>,
}

impl PublishedPatch {
    /// This patch as the modelled shape a read reports: "metadata for a patch, including
    /// information such as the KB ID, severity, classification" (`EffectivePatch$Patch`).
    pub fn to_patch(&self) -> Patch {
        Patch {
            id: Some(self.id.clone()),
            kb_number: self.kb_number.clone(),
            title: Some(self.title.clone()),
            release_date: Some(::aws_smithy_types::DateTime::from(self.release_date)),
            vendor: self.vendor.clone(),
            product_family: self.product_family.clone(),
            product: Some(self.product.clone()),
            classification: self.classification.clone(),
            msrc_severity: self.msrc_severity.clone(),
            severity: self.severity.clone(),
            language: self.language.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            release: self.release.clone(),
            arch: self.arch.clone(),
            repository: self.repository.clone(),
            advisory_ids: (!self.advisory_ids.is_empty()).then(|| self.advisory_ids.clone()),
            cve_ids: (!self.cve_ids.is_empty()).then(|| self.cve_ids.clone()),
            bugzilla_ids: (!self.bugzilla_ids.is_empty()).then(|| self.bugzilla_ids.clone()),
            ..Patch::default()
        }
    }

    /// The value this patch carries for the patch property a filter key names, or `None`
    /// when the patch carries no value for it — which is every property the operating
    /// system this patch is published for does not publish, and every key that selects on
    /// a property of another operating system's packages.
    fn property(&self, key: &str) -> Option<&str> {
        match key {
            FILTER_KEY_PATCH_SET => Some(self.patch_set),
            // A Windows patch is named by its Knowledge Base id; a patch published
            // without one is named by the catalogue's own id for it.
            FILTER_KEY_PATCH_ID => Some(self.kb_number.as_deref().unwrap_or(&self.id)),
            PROPERTY_PRODUCT => Some(&self.product),
            PROPERTY_PRODUCT_FAMILY => self.product_family.as_deref(),
            PROPERTY_CLASSIFICATION => self.classification.as_deref(),
            PROPERTY_MSRC_SEVERITY => self.msrc_severity.as_deref(),
            PROPERTY_SEVERITY => self.severity.as_deref(),
            PROPERTY_PRIORITY => self.priority.as_deref(),
            FILTER_KEY_NAME => self.name.as_deref(),
            // A package carries the epoch the catalogue publishes for its packages; a
            // patch that is no package carries none.
            FILTER_KEY_EPOCH => self.name.as_deref().map(|_| PUBLISHED_PACKAGE_EPOCH),
            FILTER_KEY_VERSION => self.version.as_deref(),
            FILTER_KEY_RELEASE => self.release.as_deref(),
            FILTER_KEY_ARCH => self.arch.as_deref(),
            FILTER_KEY_REPOSITORY => self.repository.as_deref(),
            _ => None,
        }
    }

    /// Every value this patch carries for the property `key` names: more than one for the
    /// properties the model gives a LIST (`Patch$AdvisoryIds`, `Patch$CVEIds`,
    /// `Patch$BugzillaIds`) and at most one for every other ([`Self::property`]).
    fn properties(&self, key: &str) -> Vec<&str> {
        let listed: &[String] = match key {
            FILTER_KEY_ADVISORY_ID => &self.advisory_ids,
            FILTER_KEY_CVE_ID => &self.cve_ids,
            FILTER_KEY_BUGZILLA_ID => &self.bugzilla_ids,
            _ => return self.property(key).into_iter().collect(),
        };
        listed.iter().map(String::as_str).collect()
    }

    /// Whether `value`, from a baseline's explicitly approved or explicitly rejected
    /// list, names THIS patch: "A list of explicitly approved patches for the baseline"
    /// holds patch ids, which for a Windows patch is its Knowledge Base id.
    fn is_named_by(&self, value: &str) -> bool {
        let value = value.trim();
        self.id.eq_ignore_ascii_case(value)
            || self
                .kb_number
                .as_deref()
                .is_some_and(|kb| kb.eq_ignore_ascii_case(value))
    }

    /// Whether this patch meets EVERY filter of `group`: "A patch filter consists of a
    /// key and a set of values… if the filter key is `PRODUCT` and the filter values are
    /// `["Office 2013", "Office 2016"]`, then the filter accepts all patches where
    /// product name is either "Office 2013" or "Office 2016"" (`PatchFilter`) — so a
    /// group selects a patch when each of its filters accepts it, and a filter accepts a
    /// patch when any of its values is the patch's value for that key, or is the wildcard
    /// that "matches all values".
    pub fn matches(&self, group: &PatchFilterGroup) -> bool {
        group
            .patch_filters
            .iter()
            .all(|filter| self.matches_filter(&filter.key, &filter.values))
    }

    /// Whether this patch meets ONE filter: the key it searches on and the values it
    /// accepts for it.
    ///
    /// "A patch filter consists of a key and a set of values" (`PatchFilter`), so a patch
    /// meets a filter when any of its values is the value this patch carries for that key,
    /// or is the wildcard that "matches all values" — and a key this patch carries no
    /// value for selects it not at all. The one place a filter is matched against a patch,
    /// whether it comes from a baseline's filter group or from the `Filters` of a
    /// catalogue read ([`super::available_patches`]).
    pub fn matches_filter(&self, key: &str, values: &[String]) -> bool {
        let held = self.properties(key);
        if held.is_empty() {
            return false;
        }
        values.iter().any(|value| {
            value == FILTER_VALUE_WILDCARD || held.iter().any(|carried| value == carried)
        })
    }
}

/// One patch the catalogue publishes together with the approval state one baseline gives
/// it — "metadata about a patch along with the approval state of the patch in a particular
/// patch baseline" (`EffectivePatch`).
#[derive(Debug, Clone)]
pub struct EffectivePatchRecord {
    /// The published patch this state is about.
    pub patch: PublishedPatch,
    /// "The approval status of a patch" (`PatchStatus$DeploymentStatus`): one of the
    /// `DEPLOYMENT_STATUS_*` values above.
    pub deployment_status: &'static str,
    /// "The compliance severity level for a patch" (`PatchStatus$ComplianceLevel`): the
    /// level the baseline assigns the patches it approves, and nothing for a patch it
    /// approves not at all.
    pub compliance_level: Option<String>,
    /// "The date the patch was approved (or will be approved if the status is
    /// `PENDING_APPROVAL`)" (`PatchStatus$ApprovalDate`), and nothing for a patch no
    /// approval date can be worked out for.
    pub approval_date: Option<SystemTime>,
}

impl EffectivePatchRecord {
    /// This record as the modelled shape a read reports.
    pub fn to_effective_patch(&self) -> EffectivePatch {
        EffectivePatch {
            patch: Some(self.patch.to_patch()),
            patch_status: Some(PatchStatus {
                deployment_status: Some(self.deployment_status.to_string()),
                compliance_level: self.compliance_level.clone(),
                approval_date: self
                    .approval_date
                    .map(::aws_smithy_types::DateTime::from),
            }),
        }
    }
}

/// The patches Patch Manager publishes for `operating_system`, newest release first.
///
/// One patch per published product per monthly release: the products, product families,
/// classifications and severities are the ones that operating system's own published
/// metadata holds ([`PUBLISHED_PATCH_METADATA`]), and the release instants are the last
/// [`MONTHLY_RELEASES`] monthly releases on or before `at` — so a catalogue read invents
/// no product and no property value, and a patch released after the instant a read is
/// made at is not in it.
pub fn published_patches(operating_system: &str, at: SystemTime) -> Vec<PublishedPatch> {
    let releases = monthly_releases(at, MONTHLY_RELEASES);
    let mut patches = Vec::new();
    // The ordinal of the product within this operating system's catalogue, which is what
    // makes the minted ids unique and gives each product its own position in the rotation
    // through the property values its group publishes.
    let mut ordinal: u64 = 0;
    for group in PUBLISHED_PATCH_METADATA
        .iter()
        .filter(|group| group.operating_system == operating_system)
    {
        for product in group.products {
            for release in &releases {
                patches.push(published_patch(group, product, release, ordinal));
            }
            ordinal += 1;
        }
    }
    // A total order, taken from the patches themselves rather than from the request:
    // newest release first, then by product and by the patch's own id, so two reads report
    // the same sequence and a page resumes at the patch the last one stopped at.
    patches.sort_by(|left, right| {
        right
            .release_date
            .cmp(&left.release_date)
            .then_with(|| left.product.cmp(&right.product))
            .then_with(|| left.id.cmp(&right.id))
    });
    patches
}

/// Which of the patches published for `baseline`'s operating system that baseline makes
/// effective, and the approval state it gives each of them.
///
/// A READ of the baseline: what the write of the baseline persisted — its explicitly
/// approved and rejected patches, its global filters, its approval rules and the
/// compliance level it assigns — is what decides each patch's state, together with the
/// patch's own release date, so nothing is stored beside the catalogue and a later read
/// reports what the last write of the baseline implies.
///
/// A patch the baseline says nothing about is not effective in it at all and is left out:
/// "a set of global filters used to include patches in the baseline"
/// (`CreatePatchBaseline$GlobalFilters`) and "a set of rules used to include patches in the
/// baseline" (`CreatePatchBaseline$ApprovalRules`) are what put a patch in a baseline,
/// beside the two lists that name patches one by one.
pub fn effective_patches(
    baseline: &PatchBaselineRecord,
    at: SystemTime,
) -> Vec<EffectivePatchRecord> {
    published_patches(&baseline.operating_system, at)
        .into_iter()
        .filter_map(|patch| effective_patch(baseline, patch, at))
        .collect()
}

/// The approval state `baseline` gives one published patch, or `None` when that baseline
/// does not make the patch effective at all.
fn effective_patch(
    baseline: &PatchBaselineRecord,
    patch: PublishedPatch,
    at: SystemTime,
) -> Option<EffectivePatchRecord> {
    // A patch the baseline names one by one carries the state the baseline NAMED,
    // whatever its filters and rules select: an explicit rejection is the strongest
    // statement a baseline makes about a patch — the patch is to be left alone even where
    // a rule would approve it — so it is read before the explicit approval and before the
    // rules. A rejected patch is never approved, so it carries neither an approval date
    // nor the compliance level the baseline assigns the patches it approves; what
    // `RejectedPatchesAction` says is to be done with it where another package depends on
    // it is about installing the patch, not about its approval state.
    if baseline
        .rejected_patches
        .iter()
        .any(|named| patch.is_named_by(named))
    {
        return Some(EffectivePatchRecord {
            patch,
            deployment_status: DEPLOYMENT_STATUS_EXPLICIT_REJECTED,
            compliance_level: None,
            approval_date: None,
        });
    }
    if baseline
        .approved_patches
        .iter()
        .any(|named| patch.is_named_by(named))
    {
        return Some(EffectivePatchRecord {
            patch,
            deployment_status: DEPLOYMENT_STATUS_EXPLICIT_APPROVED,
            // "Defines the compliance level for approved patches"
            // (`CreatePatchBaseline$ApprovedPatchesComplianceLevel`) — the level the
            // baseline assigns the patches it approves by naming them.
            compliance_level: Some(baseline.approved_patches_compliance_level.clone()),
            // The patch was approved when the write that named it ran: nothing has to
            // elapse for a patch a baseline approves explicitly.
            approval_date: Some(baseline.modified_date),
        });
    }

    // The global filters narrow which patches the baseline's rules are applied to at all,
    // so a patch outside them is in the baseline only if one of the two lists named it.
    if let Some(global) = baseline.global_filters.as_ref() {
        if !global.patch_filters.is_empty() && !patch.matches(global) {
            return None;
        }
    }

    // "The patch filter group that defines the criteria for the rule" (`PatchRule`): the
    // FIRST rule whose criteria the patch meets is the rule that approves it, so a
    // baseline whose rules overlap approves a patch once, under the rule it stated first.
    let rule = baseline
        .approval_rules
        .as_ref()?
        .patch_rules
        .iter()
        .find(|rule| patch.matches(&rule.patch_filter_group))?;
    let compliance_level = Some(
        rule.compliance_level
            .clone()
            .unwrap_or_else(|| DEFAULT_APPROVED_PATCHES_COMPLIANCE_LEVEL.to_string()),
    );

    // "The number of days after the release date of each patch matched by the rule that
    // the patch is marked as approved in the patch baseline. For example, a value of 7
    // means that patches are approved seven days after they are released"
    // (`PatchRule$ApproveAfterDays`) — so the date the patch is approved on is its own
    // release date moved on by that many days, and the patch is approved once that date
    // has passed and due to be approved on it until then.
    if let Some(days) = rule.approve_after_days {
        // The window is bounded by the modelled range of the member (0..360), and the
        // arithmetic is still checked: a release instant moved on by a window is an
        // instant the clock type may not hold, and a date nothing can work out is
        // reported as no date rather than ending the request.
        let approval_date = u32::try_from(days)
            .ok()
            .and_then(|days| DAY.checked_mul(days))
            .and_then(|window| patch.release_date.checked_add(window));
        let deployment_status = match approval_date {
            Some(approval_date) if approval_date <= at => DEPLOYMENT_STATUS_APPROVED,
            _ => DEPLOYMENT_STATUS_PENDING_APPROVAL,
        };
        return Some(EffectivePatchRecord {
            patch,
            deployment_status,
            compliance_level,
            approval_date,
        });
    }

    // "The cutoff date for auto approval of released patches. Any patches released on or
    // before this date are installed automatically" (`PatchRule$ApproveUntilDate`): a
    // patch of the rule's criteria released by the cutoff was approved on its release,
    // and one released after it is selected by the rule but approved by nothing, so no
    // date can be worked out for it.
    let approved_by_cutoff = rule
        .approve_until_date
        .as_deref()
        .and_then(approval_cutoff)
        .is_some_and(|cutoff| patch.release_date <= cutoff);
    if approved_by_cutoff {
        let approval_date = patch.release_date;
        return Some(EffectivePatchRecord {
            patch,
            deployment_status: DEPLOYMENT_STATUS_APPROVED,
            compliance_level,
            approval_date: Some(approval_date),
        });
    }
    Some(EffectivePatchRecord {
        patch,
        deployment_status: DEPLOYMENT_STATUS_PENDING_APPROVAL,
        compliance_level,
        approval_date: None,
    })
}

/// The last instant a patch may have been released at to be approved by a rule whose
/// cutoff is `date`: "Enter dates in the format `YYYY-MM-DD`… If you enter the date
/// `2025-11-16`, patches released between `2025-11-16T00:00:00Z` and
/// `2025-11-16T23:59:59Z` will be included in the approval"
/// (`PatchRule$ApproveUntilDate`), so the cutoff is the END of the day it names, read in
/// UTC the way every patch release date is.
fn approval_cutoff(date: &str) -> Option<SystemTime> {
    let day = ::chrono::NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").ok()?;
    let end = day.and_hms_opt(23, 59, 59)?.and_utc();
    Some(SystemTime::from(end))
}

/// One monthly release of a published catalogue.
#[derive(Debug, Clone, Copy)]
struct MonthlyRelease {
    /// The instant the release was published at.
    at: SystemTime,
    /// The month the release belongs to, counted in whole months from the start of the
    /// calendar: what makes a minted patch id unique across releases and what rotates the
    /// property values a release's patches carry.
    month_ordinal: u64,
    /// The calendar year of the release, which its patches' titles carry.
    year: i32,
    /// The calendar month of the release, which its patches' titles carry.
    month: u32,
}

/// The last `count` monthly releases published on or before `at`, newest first.
fn monthly_releases(at: SystemTime, count: usize) -> Vec<MonthlyRelease> {
    let Some(now) = calendar_instant(at) else {
        return Vec::new();
    };
    let months_from_zero = i64::from(::chrono::Datelike::year(&now)) * 12
        + i64::from(::chrono::Datelike::month0(&now));
    let mut releases = Vec::new();
    // The release of the current month may not have been published yet, so one month more
    // than the catalogue holds is walked back over and only the published ones are kept.
    for back in 0..=count {
        let Some(release) = monthly_release(months_from_zero - back as i64) else {
            continue;
        };
        if release.at <= at {
            releases.push(release);
        }
        if releases.len() == count {
            break;
        }
    }
    releases
}

/// The release of the month `months_from_zero` whole months after the start of the
/// calendar, or `None` for a month no calendar date exists for.
fn monthly_release(months_from_zero: i64) -> Option<MonthlyRelease> {
    let year = i32::try_from(months_from_zero.div_euclid(12)).ok()?;
    let month = u32::try_from(months_from_zero.rem_euclid(12)).ok()? + 1;
    let first = ::chrono::NaiveDate::from_ymd_opt(year, month, 1)?;
    // The weekday the first of the month falls on decides how far into the month the
    // first release weekday is, and the release is the one a whole week after it.
    let weekday = ::chrono::Datelike::weekday(&first);
    let to_first = (RELEASE_WEEKDAY.num_days_from_monday() + 7 - weekday.num_days_from_monday()) % 7;
    let day = 1 + to_first + 7 * (RELEASE_WEEK - 1);
    let date = ::chrono::NaiveDate::from_ymd_opt(year, month, day)?;
    let at = date.and_hms_opt(RELEASE_HOUR_UTC, 0, 0)?.and_utc();
    Some(MonthlyRelease {
        at: SystemTime::from(at),
        month_ordinal: u64::try_from(months_from_zero).ok()?,
        year,
        month,
    })
}

/// The value at `position` of a published property's values, cycling through them, or
/// `None` when the operating system publishes no value for that property at all.
fn rotated(values: &'static [&'static str], position: u64) -> Option<&'static str> {
    if values.is_empty() {
        return None;
    }
    let index = usize::try_from(position % values.len() as u64).ok()?;
    values.get(index).copied()
}

/// One published patch: the patch of one product in one monthly release.
///
/// Every property it carries is the value `group`'s published metadata holds for it at this
/// patch's position in that group's rotation, so the catalogue invents no property value
/// and a later read reports the patch an earlier read reported.
fn published_patch(
    group: &PublishedPatchGroup,
    product: &'static str,
    release: &MonthlyRelease,
    ordinal: u64,
) -> PublishedPatch {
    let operating_system = group.operating_system;
    // This patch's position in the rotation through the values its group publishes: the
    // product's own ordinal moved on by the release, so each product has its own sequence
    // and each release moves every product on.
    let position = ordinal + release.month_ordinal;
    let classification = rotated(group.classifications, position);
    let windows = operating_system == PATCH_SET_OPERATING_SYSTEM;
    // "The Microsoft Knowledge Base ID of the patch. Applies to Windows patches only":
    // one id per product per release, which is what makes it unique within the catalogue.
    let kb_number = windows.then(|| {
        format!(
            "KB{}",
            KB_NUMBER_BASE + (release.month_ordinal % 1_000) * 100 + ordinal % 100
        )
    });
    let id = patch_id(operating_system, product, release.month_ordinal);
    let mut title = format!(
        "{}-{:02} {} for {}",
        release.year,
        release.month,
        classification.unwrap_or("Update"),
        product
    );
    if let Some(kb) = kb_number.as_deref() {
        let _ = write!(title, " ({kb})");
    }
    // The package this patch updates, where the operating system publishes its patches as
    // packages: its name, version, release, architecture and repository are the package
    // properties "Applies to Linux-based managed nodes only" names.
    let package = rotated(group.packages, position);
    // A patch published through a security advisory carries that advisory and what it
    // addresses; another update kind is not published through one.
    let advisory = (classification == Some(SECURITY_CLASSIFICATION)
        && !group.advisory_prefix.is_empty())
        .then(|| advisory_id(group.advisory_prefix, release, position));
    PublishedPatch {
        id,
        kb_number,
        title,
        release_date: release.at,
        vendor: windows.then(|| WINDOWS_VENDOR.to_string()),
        product_family: (!group.product_family.is_empty())
            .then(|| group.product_family.to_string()),
        product: product.to_string(),
        classification: classification.map(str::to_string),
        msrc_severity: rotated(group.msrc_severities, position).map(str::to_string),
        severity: rotated(group.severities, ordinal + 3 * release.month_ordinal)
            .map(str::to_string),
        priority: rotated(group.priorities, position).map(str::to_string),
        language: windows.then(|| LANGUAGE_NEUTRAL.to_string()),
        patch_set: group.patch_set,
        name: package.map(str::to_string),
        version: package.map(|_| package_version(position)),
        release: package
            .filter(|_| !group.release_tag.is_empty())
            .map(|_| package_release(group.release_tag, release)),
        arch: rotated(group.architectures, position).map(str::to_string),
        repository: rotated(group.repositories, position).map(str::to_string),
        cve_ids: advisory
            .iter()
            .map(|_| vulnerability_id(release, position))
            .collect(),
        bugzilla_ids: advisory.iter().map(|_| bug_id(position)).collect(),
        advisory_ids: advisory.into_iter().collect(),
    }
}

/// The version of a published package: "the version number of the patch. For example, in
/// `example-pkg-1.710.10-2.7.abcd.x86_64`, the version number is indicated by `-1`"
/// (`Patch$Version`) — three dot-separated numbers, as the filter's own sample values are
/// (`VERSION` Sample values: `78.6.1` | `4.10.16`), derived from the patch's position in
/// the catalogue so it is stable across reads.
fn package_version(position: u64) -> String {
    format!("{}.{}.{}", position % 7 + 1, position % 13, position % 97)
}

/// The release of a published package: "in `pkg-example-EE-20180914-2.2.amzn1.noarch`, the
/// release is `2.amzn1`" (`Patch$Release`) — a release number and the distribution's own
/// tag ([`PublishedPatchGroup::release_tag`]), as the filter's sample values are (`RELEASE`
/// Sample values: `9.56.amzn1` | `1.amzn2`).
fn package_release(release_tag: &str, release: &MonthlyRelease) -> String {
    format!("{}.{release_tag}", release.month)
}

/// The advisory a security patch is published through: the distribution's own advisory
/// prefix, the year of the release and a sequence number, as the filter's sample values are
/// (`ADVISORY_ID` Sample values: `ALAS-2018-1058` | `ALAS2-2021-1594`).
///
/// Minted from the patch's own position in the catalogue, the way the Knowledge Base id of
/// a Windows patch is, so it is stable across reads and unique within a year's advisories.
fn advisory_id(prefix: &str, release: &MonthlyRelease, position: u64) -> String {
    format!("{prefix}-{}-{:04}", release.year, 1_000 + position % 9_000)
}

/// A vulnerability that advisory addresses, in the form the filter's sample values carry
/// (`CVE_ID` Sample values: `CVE-2018-3615` | `CVE-2020-1472`), minted from the same
/// position.
fn vulnerability_id(release: &MonthlyRelease, position: u64) -> String {
    format!("CVE-{}-{:04}", release.year, 1_000 + position * 7 % 9_000)
}

/// A bug it closes, in the form the filter's sample value carries (`BUGZILLA_ID` Sample
/// values: `1463241`), minted from the same position.
fn bug_id(position: u64) -> String {
    format!("{}", 1_400_000 + position * 13 % 100_000)
}

/// The catalogue's own id for the patch of `product` in one release: "The ID of the
/// patch… This ID isn't the same as the Microsoft Knowledge Base ID" (`Patch$Id`), so it
/// is derived from the patch's identity rather than from the id the vendor publishes —
/// which makes it stable across reads and unique within the catalogue, and keeps it inside
/// the modelled `PatchId` length (1..100).
fn patch_id(operating_system: &str, product: &str, month_ordinal: u64) -> String {
    let mut digest = Sha256::new();
    digest.update(operating_system.as_bytes());
    digest.update([0]);
    digest.update(product.as_bytes());
    digest.update([0]);
    digest.update(month_ordinal.to_be_bytes());
    let rendered = ::hex::encode(digest.finalize());
    format!(
        "{}-{}-{}-{}-{}",
        &rendered[0..8],
        &rendered[8..12],
        &rendered[12..16],
        &rendered[16..20],
        &rendered[20..32]
    )
}
