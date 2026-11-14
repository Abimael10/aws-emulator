// SPDX-License-Identifier: CC0-1.0

//! The account's SOFTWARE PACKAGE CONFIGURATION: the one record
//! `UpdatePackageConfiguration` writes and `GetPackageConfiguration` reports.
//!
//! Neither operation names a resource — both are bound to `/package-configuration`
//! with no path label and the reader takes no input at all
//! (`smithy_query(query="operation",operation="GetPackageConfiguration,UpdatePackageConfiguration")`)
//! — so the account holds ONE such record rather than a map of them, and it is a plain
//! record and not an `Option`: an account nobody has configured still HAS a
//! configuration, namely the version reporting turned off, which is what keeps the
//! reader a pure read instead of a call that has to create the record it reports.
//!
//! The record holds what `VersionUpdateByJobsConfig` carries — "Configuration to manage
//! IoT Job's package version reporting", whose `enabled` "indicates whether the Job is
//! enabled or not" and whose `roleArn` is "the role that grants permission to the IoT
//! jobs service to update the reserved named shadow when the job successfully
//! completes" (`smithy_query(query="shape",shape="VersionUpdateByJobsConfig")`) — so the
//! reader hands back exactly what the last update recorded and the writer and the
//! reader cannot drift.
//!
//! `enabled` is kept as a plain `bool` because the account is either reporting versions
//! or not, while `role_arn` stays an `Option`: a configuration that names no role HAS
//! none, which is distinguishable from one that names it.

use crate::ops::types::VersionUpdateByJobsConfig;

/// The account's software package configuration, as this service persists it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageConfiguration {
    /// Whether IoT Jobs reports a deployed package version into the thing's reserved
    /// named shadow. An account nobody has configured does not — [`Default`].
    pub version_update_by_jobs_enabled: bool,
    /// The role IoT Jobs assumes to write that shadow, once an update has named one.
    pub version_update_by_jobs_role_arn: Option<String>,
}

impl PackageConfiguration {
    /// Apply one `UpdatePackageConfiguration` request's `versionUpdateByJobsConfig`.
    ///
    /// Each member the request supplies replaces the stored value and one it leaves out
    /// leaves the stored value alone, which is what makes this a PATCH of the record
    /// (`PATCH /package-configuration`) rather than a re-creation of it: a request that
    /// carries only `enabled` turns reporting on or off without discarding the role the
    /// account already configured.
    pub fn apply(&mut self, requested: &VersionUpdateByJobsConfig) {
        if let Some(enabled) = requested.enabled {
            self.version_update_by_jobs_enabled = enabled;
        }
        if let Some(role_arn) = requested.role_arn.as_ref() {
            self.version_update_by_jobs_role_arn = Some(role_arn.clone());
        }
    }

    /// This record as the `versionUpdateByJobsConfig` every reader of it reports.
    pub fn version_update_by_jobs_config(&self) -> VersionUpdateByJobsConfig {
        VersionUpdateByJobsConfig {
            enabled: Some(self.version_update_by_jobs_enabled),
            role_arn: self.version_update_by_jobs_role_arn.clone(),
        }
    }
}
