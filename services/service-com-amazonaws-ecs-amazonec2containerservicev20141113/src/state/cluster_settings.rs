// SPDX-License-Identifier: CC0-1.0

//! The cluster SETTINGS a cluster carries: the ones its create named, and the account
//! setting of every other name the model defines, captured as that create resolved it.
//!
//! # The rule this module implements
//!
//! `ClusterSetting$value`: "If a cluster value is specified, it will override the
//! `containerInsights` value set with PutAccountSetting or PutAccountSettingDefault" — so a
//! create that names no value for a setting takes the account one, and it takes it ONCE, at
//! the instant the cluster is created. `Cluster$settings` is part of the cluster's own
//! configuration, which only an operation that acts on THAT cluster changes
//! (`UpdateClusterSettings` "Modifies the settings to use for a cluster",
//! `UpdateCluster$settings`), so a later account-setting write decides what a cluster created
//! after it inherits and leaves every cluster that already exists exactly as it stands.
//!
//! That is why the inheritance lives HERE, on the create path
//! ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::captured_cluster_settings`]),
//! and every reader of a cluster's settings — `DescribeClusters`, the answers of the
//! cluster-mutating operations and the Container Insights publisher
//! ([`super::container_insights`]) — reads the stored record
//! ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::cluster_settings_view`])
//! rather than the account-settings store, so no read of a cluster can answer differently
//! from one moment to the next.

use super::*;

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// The settings to STORE on a cluster being created with `requested`: every setting the
    /// request named, plus one entry for each `ClusterSettingName` it left out, carrying the
    /// account setting of that name in force for the caller at this instant.
    ///
    /// This is the one place a cluster inherits an account setting, so the value it inherits
    /// is fixed by the create and nothing later re-resolves it.
    pub fn captured_cluster_settings(&self, requested: &[ClusterSetting]) -> Vec<ClusterSetting> {
        let principal_arn = caller_identity_arn();
        let mut settings = requested.to_vec();
        for name in CLUSTER_SETTING_NAME_VALUES {
            if requested
                .iter()
                .any(|setting| setting.name.as_deref() == Some(*name))
            {
                continue;
            }
            settings.push(ClusterSetting {
                name: Some((*name).to_string()),
                value: Some(self.effective_account_setting(&principal_arn, name).value),
            });
        }
        settings
    }

    /// The `Cluster$settings` list a cluster is REPORTED with: one entry for every setting
    /// name the model defines (`ClusterSettingName`), carrying the value the cluster record
    /// holds for it.
    ///
    /// `Cluster$settings` "indicates whether CloudWatch Container Insights is on or off for a
    /// cluster", so every cluster has an answer to that question — the one its create
    /// captured ([`Self::captured_cluster_settings`]) and its own later updates wrote. A
    /// record that holds no value for a name at all is reported with that setting's own
    /// documented default ([`ACCOUNT_SETTINGS`]) rather than with the account value in force
    /// now, because what a cluster reports is the cluster's, not the account's.
    pub fn cluster_settings_view(&self, cluster: &ClusterRecord) -> Vec<ClusterSetting> {
        CLUSTER_SETTING_NAME_VALUES
            .iter()
            .map(|name| ClusterSetting {
                name: Some((*name).to_string()),
                value: Some(cluster.setting_value(name).unwrap_or_else(|| {
                    account_setting_spec(name)
                        // Unreachable for a `name` off the closed `ClusterSettingName`
                        // enum, every one of which is an account setting too.
                        .map_or(CLUSTER_SETTING_DEFAULT_VALUE, |setting| {
                            setting.default_value
                        })
                        .to_string()
                })),
            })
            .collect()
    }
}
