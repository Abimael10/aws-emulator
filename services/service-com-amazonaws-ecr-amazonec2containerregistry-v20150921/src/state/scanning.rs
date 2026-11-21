// SPDX-License-Identifier: CC0-1.0

//! The registry's scanning configuration: the record `PutRegistryScanningConfiguration`
//! stores, `GetRegistryScanningConfiguration` reports, and
//! `BatchGetRepositoryScanningConfiguration` reports the effect of on one repository.
//!
//! It lives beside [`super`]'s repository state rather than inside it: there is one
//! scanning configuration per registry, and this emulator serves one registry, so the
//! record is a single optional field on the service state
//! ([`super::ComAmazonawsEcrAmazonec2containerregistryV20150921State::registry_scanning`])
//! and everything that derives from it is here.
//!
//! The scan frequencies a scan type accepts are the model's own, not this emulator's
//! invention — see [`supported_scan_frequencies`]. `CONTINUOUS_SCAN` is therefore
//! reachable now: the `ENHANCED` scan type it belongs to is what a put may set.

use crate::ops::types::{
    RegistryScanningConfiguration, RegistryScanningRule, RepositoryScanningConfiguration,
    ScanningRepositoryFilter,
};

use super::{Fault, Repo, MANUAL_SCAN_FREQUENCY, SCAN_ON_PUSH_FREQUENCY};

/// The scan type a registry reports while no scanning configuration has been stored for
/// it: "When a registry scanning configuration is not defined, by default the BASIC scan
/// type is used" (`PutRegistryScanningConfigurationRequest$scanType`).
pub const DEFAULT_SCAN_TYPE: &str = BASIC_SCAN_TYPE;

/// The scan type Amazon ECR performs itself; see [`supported_scan_frequencies`].
pub const BASIC_SCAN_TYPE: &str = "BASIC";

/// The scan type Amazon Inspector performs; see [`supported_scan_frequencies`].
pub const ENHANCED_SCAN_TYPE: &str = "ENHANCED";

/// The `ScanFrequency` that belongs to [`ENHANCED_SCAN_TYPE`] scanning, where Amazon
/// Inspector scans continuously rather than on a push; see
/// [`supported_scan_frequencies`].
pub const CONTINUOUS_SCAN_FREQUENCY: &str = "CONTINUOUS_SCAN";

impl super::ComAmazonawsEcrAmazonec2containerregistryV20150921State {
    /// The registry's scanning configuration as every reader of it reports it: what the
    /// last `PutRegistryScanningConfiguration` stored, or — while nothing is stored — the
    /// documented default, the `BASIC` scan type with no scanning rules.
    pub fn registry_scanning_configuration(&self) -> RegistryScanningConfiguration {
        self.registry_scanning
            .clone()
            .unwrap_or_else(|| RegistryScanningConfiguration {
                scan_type: Some(DEFAULT_SCAN_TYPE.to_string()),
                rules: Some(Vec::new()),
            })
    }

    /// The scanning configuration reported for the repository `repo`:
    /// `BatchGetRepositoryScanningConfiguration`'s answer for one repository.
    ///
    /// A scanning configuration is installed at the REGISTRY and applies to the
    /// repositories its rules name, so what a repository reports is the registry
    /// configuration in force ON IT, read off the stored record rather than defaulted:
    ///
    /// * `appliedScanFilters` are the repository filters of the registry's rules that
    ///   cover this repository ([`rule_covers`]) — the filters applied to it, which is
    ///   empty exactly when no stored filter names it;
    /// * `scanFrequency` is the frequency of the first such rule, in the order the
    ///   configuration lists its rules, because that is the instruction in force on the
    ///   repository; with no rule covering it, the frequency follows from the repository's
    ///   own `imageScanningConfiguration` — `SCAN_ON_PUSH` for one scanned on push and
    ///   otherwise `MANUAL`, the frequency the model documents as the default ("If scan on
    ///   push is not specified, then the `MANUAL` scan frequency is set by default",
    ///   `RegistryScanningRule$scanFrequency`);
    /// * `scanOnPush` is whether an image pushed into this repository is scanned on
    ///   arrival, which either standing instruction can ask for: the repository's own flag
    ///   or a covering rule asking for the `SCAN_ON_PUSH` frequency.
    pub fn repository_scanning_configuration(
        &self,
        repo: &Repo,
    ) -> RepositoryScanningConfiguration {
        let configuration = self.registry_scanning_configuration();
        let rules = configuration.rules.unwrap_or_default();
        let covering: Vec<&RegistryScanningRule> = rules
            .iter()
            .filter(|rule| rule_covers(rule, &repo.name))
            .collect();
        let applied_scan_filters: Vec<ScanningRepositoryFilter> = covering
            .iter()
            .flat_map(|rule| rule.repository_filters.iter())
            .filter(|filter| super::wildcard_matches(&filter.filter, &repo.name))
            .cloned()
            .collect();
        let scan_on_push = repo.scans_on_push()
            || covering
                .iter()
                .any(|rule| rule.scan_frequency == SCAN_ON_PUSH_FREQUENCY);
        let scan_frequency = match covering.first() {
            Some(rule) => rule.scan_frequency.clone(),
            None if repo.scans_on_push() => SCAN_ON_PUSH_FREQUENCY.to_string(),
            None => MANUAL_SCAN_FREQUENCY.to_string(),
        };
        RepositoryScanningConfiguration {
            repository_arn: Some(repo.arn.clone()),
            repository_name: Some(repo.name.clone()),
            scan_on_push,
            scan_frequency: Some(scan_frequency),
            applied_scan_filters: Some(applied_scan_filters),
        }
    }
}

/// Whether the registry scanning `rule` covers the repository named `repository_name`, so
/// that its frequency is the instruction in force on that repository.
///
/// A rule carrying no `repositoryFilters` covers every repository of the registry — the
/// reading the sibling registry-wide configuration documents for its own filters ("if no
/// filters are added, the contents of all repositories are replicated",
/// `RepositoryFilter`) — and a rule with filters covers the repositories any one of them
/// matches, as the wildcard its only `filterType` names ([`super::wildcard_matches`], this
/// crate's one wildcard matcher). The same reading as the push side takes
/// ([`super::ComAmazonawsEcrAmazonec2containerregistryV20150921State::image_is_scanned_on_push`]),
/// so what a repository REPORTS and what a push into it does cannot disagree.
pub fn rule_covers(rule: &RegistryScanningRule, repository_name: &str) -> bool {
    rule.repository_filters.is_empty()
        || rule
            .repository_filters
            .iter()
            .any(|filter| super::wildcard_matches(&filter.filter, repository_name))
}

/// The scan frequencies a scan type supports, from
/// `RegistryScanningRule$scanFrequency`: "When the `ENHANCED` scan type is specified, the
/// supported scan frequencies are `CONTINUOUS_SCAN` and `SCAN_ON_PUSH`. When the `BASIC`
/// scan type is specified, the `SCAN_ON_PUSH` scan frequency is supported. If scan on
/// push is not specified, then the `MANUAL` scan frequency is set by default." — so
/// `MANUAL` belongs to basic scanning, which is the type that has it as its default, and
/// `CONTINUOUS_SCAN` only to enhanced scanning.
///
/// A scan type the model does not declare cannot reach here: `ScanType` is an enum, so
/// the boundary refuses any other value before a handler runs.
pub fn supported_scan_frequencies(scan_type: &str) -> &'static [&'static str] {
    match scan_type {
        ENHANCED_SCAN_TYPE => &[CONTINUOUS_SCAN_FREQUENCY, SCAN_ON_PUSH_FREQUENCY],
        // BASIC, and anything the `ScanType` enum is widened with later.
        _ => &[MANUAL_SCAN_FREQUENCY, SCAN_ON_PUSH_FREQUENCY],
    }
}

/// The configuration a `PutRegistryScanningConfiguration` request stores: its `scanType`
/// (defaulted to [`DEFAULT_SCAN_TYPE`], since a request that names none leaves the
/// registry on the default scan type) and its rules, refused with
/// [`Fault::InvalidParameter`] when a rule asks for a frequency the scan type does not
/// support.
///
/// Every per-member constraint — the two-rule maximum, the hundred-filter maximum, the
/// `filter` pattern and length, the `filterType` and `scanFrequency` enums — is modelled,
/// so the boundary has already refused a request that breaks one. What is left is the
/// pairing of `scanType` with `scanFrequency`, which the model cannot express.
pub fn scanning_configuration_from_request(
    scan_type: Option<String>,
    rules: Option<Vec<RegistryScanningRule>>,
) -> Result<RegistryScanningConfiguration, Fault> {
    let scan_type = scan_type.unwrap_or_else(|| DEFAULT_SCAN_TYPE.to_string());
    let rules = rules.unwrap_or_default();
    let supported = supported_scan_frequencies(&scan_type);
    for rule in &rules {
        if !supported.contains(&rule.scan_frequency.as_str()) {
            return Err(Fault::InvalidParameter(format!(
                "scanFrequency '{}' is not supported with the '{scan_type}' scan type; \
                 the supported scan frequencies are {}",
                rule.scan_frequency,
                supported.join(", ")
            )));
        }
    }
    Ok(RegistryScanningConfiguration {
        scan_type: Some(scan_type),
        rules: Some(rules),
    })
}
