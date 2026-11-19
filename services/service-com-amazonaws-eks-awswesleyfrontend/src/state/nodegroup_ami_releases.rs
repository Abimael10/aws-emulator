// SPDX-License-Identifier: CC0-1.0

//! The Amazon EKS optimized AMI releases a managed node group is deployed with.
//!
//! "All node groups are created with the latest AMI release version for the
//! respective minor Kubernetes version of the cluster, unless you deploy a custom
//! AMI using a launch template" (`CreateNodegroup`), and the member a node group
//! reports it as is "the version of the Amazon EKS optimized AMI that the node
//! group was deployed with" (`Nodegroup$releaseVersion`) — "by default, the latest
//! available AMI version for the node group's current Kubernetes version"
//! (`CreateNodegroupRequest$releaseVersion`).
//!
//! An AMI release is therefore PUBLISHED data, like the Kubernetes version
//! calendar ([`ClusterVersionRecord`]) and the add-on catalog: Amazon EKS builds
//! an optimized AMI for a Kubernetes version and names the build after that
//! version and the day it was built. This module publishes one AMI release line
//! per row of that calendar, read off the row itself, so every Kubernetes version
//! a cluster can run has exactly one latest AMI release, a version the calendar
//! does not carry has none, and widening the calendar publishes the AMI releases
//! of its new rows with it.

use super::*;

/// The build an AMI release names between its Kubernetes version and its build
/// day.
///
/// An AMI release version is `<kubernetesVersion>.<build>-<YYYYMMDD>`: the
/// Kubernetes version the AMI runs, which build of that version's AMI line it is,
/// and the day that build was made. One build per line is published here, so it
/// is that line's first.
const AMI_RELEASE_BUILD: u32 = 0;

/// How many digits the day an AMI release names itself after has: `YYYYMMDD`.
const AMI_RELEASE_DAY_DIGITS: usize = 8;

/// The release version of the Amazon EKS optimized AMI published for
/// `kubernetes_version` and built on the day `built_at` falls on.
///
/// `None` when that day cannot be read at all, so no node group reports a release
/// version with a day missing from it.
#[must_use]
pub fn ami_release_version(kubernetes_version: &str, built_at: SystemTime) -> Option<String> {
    // The day out of the `YYYY-MM-DDThh:mm:ssZ` form the instant is written in,
    // with the separators dropped: the `YYYYMMDD` an AMI release carries.
    let day: String = kubernetes_objects::rfc3339_seconds(built_at)
        .chars()
        .take_while(|character| *character != 'T')
        .filter(char::is_ascii_digit)
        .collect();
    if day.len() != AMI_RELEASE_DAY_DIGITS {
        return None;
    }
    Some(format!("{kubernetes_version}.{AMI_RELEASE_BUILD}-{day}"))
}

impl ComAmazonawsEksAwswesleyfrontendState {
    /// The latest AMI release version published for `kubernetes_version`, read
    /// off that version's own calendar row: Amazon EKS publishes the optimized
    /// AMI for a Kubernetes version with that version, so the row's release date
    /// is the day its AMI line was built.
    ///
    /// `None` for a Kubernetes version this service publishes no calendar row
    /// for, which is a version no cluster of it runs: nothing is reported for an
    /// AMI line that was never published.
    #[must_use]
    pub fn latest_ami_release_version(&self, kubernetes_version: &str) -> Option<String> {
        let row = self
            .cluster_versions
            .iter()
            .find(|row| row.cluster_version == kubernetes_version)?;
        ami_release_version(&row.cluster_version, row.release_date)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule: a published AMI release version is the Kubernetes version, the
    /// build of its AMI line and the day that build was made.
    #[test]
    fn an_ami_release_version_names_its_version_build_and_day() {
        // The table spans what the rule generalises over: the day's position in
        // the month, a two-digit and a one-digit month, and a Kubernetes version
        // either end of the published calendar.
        let cases = [
            ("the first row's version", "1.23", "2022-08-11", "1.23.0-20220811"),
            ("a two-digit month", "1.31", "2024-12-01", "1.31.0-20241201"),
            ("a one-digit month and day", "1.37", "2026-01-02", "1.37.0-20260102"),
        ];
        for (case, version, day, expected) in cases {
            let built_at = calendar_instant(day);
            assert_eq!(
                ami_release_version(version, built_at).as_deref(),
                Some(expected),
                "case {case:?}: expected {expected:?}, got {:?}",
                ami_release_version(version, built_at)
            );
        }
    }

    /// The rule reaches every row of the published calendar: each Kubernetes
    /// version it carries has exactly one latest AMI release, named after that
    /// version, and a version it does not carry has none.
    #[test]
    fn every_published_cluster_version_has_one_ami_release() {
        let store = ComAmazonawsEksAwswesleyfrontendState::default();
        for row in &store.cluster_versions {
            let version = row.cluster_version.as_str();
            let published = store.latest_ami_release_version(version);
            let Some(published) = published else {
                panic!("the calendar row {version:?} must publish an AMI release, got None");
            };
            assert!(
                published.starts_with(&format!("{version}.")),
                "the AMI release of {version:?} names that Kubernetes version, got {published:?}"
            );
            assert_eq!(
                store.latest_ami_release_version(version).as_deref(),
                Some(published.as_str()),
                "the AMI release of {version:?} is the same on every read, got {:?}",
                store.latest_ami_release_version(version)
            );
        }
        for absent in ["0.1", "1.1", "9.99", "not-a-version"] {
            assert_eq!(
                store.latest_ami_release_version(absent),
                None,
                "no AMI release is published for {absent:?}, which the calendar does not carry"
            );
        }
    }
}
