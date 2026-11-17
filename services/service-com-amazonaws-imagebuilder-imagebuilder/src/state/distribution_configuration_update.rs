// SPDX-License-Identifier: CC0-1.0

//! How an UPDATE of a distribution configuration is held.
//!
//! `UpdateDistributionConfiguration` "doesn't support selective updates. The request replaces
//! the stored configuration, so include every setting that you want to keep", and its
//! `distributions` member documents "This list replaces the configuration's existing
//! distributions entirely" — so an update does not merge into the stored record, it REPLACES
//! it: the settings the request describes are written as submitted, the optional ones it
//! omits are cleared, and the members an update request cannot carry (the name the ARN was
//! derived from, the resource's own tags, the instant it was created) are what the
//! configuration keeps.
//!
//! Beside the record it carries the two things the operation's own `clientToken` needs, kept
//! apart from the create's: an update is a different operation, so a token spent on one never
//! answers for the other ([`UPDATE_DISTRIBUTION_CONFIGURATION_OPERATION`]).

use super::*;

use crate::ops::types::UpdateDistributionConfigurationInput;

/// See [`CREATE_INFRASTRUCTURE_CONFIGURATION_OPERATION`]. An update carries a `clientToken`
/// of its own, so its receipts are filed under its own name and never read the create's: the
/// same token may create a configuration and then update it.
pub const UPDATE_DISTRIBUTION_CONFIGURATION_OPERATION: &str = "UpdateDistributionConfiguration";

impl ComAmazonawsImagebuilderImagebuilderState {
    /// Replace the stored configuration with `record` and file the receipt for the update
    /// that wrote it.
    ///
    /// The key is the record's own ARN, which an update never changes, so the replacement
    /// takes the place of what it replaced rather than adding a second entry.
    pub fn replace_distribution_configuration(
        &mut self,
        record: DistributionConfigurationRecord,
        client_token: &str,
        fingerprint: String,
        request_id: String,
    ) {
        self.file_create_receipt(
            UPDATE_DISTRIBUTION_CONFIGURATION_OPERATION,
            &record.region,
            client_token,
            fingerprint,
            request_id,
            record.arn.clone(),
        );
        self.distribution_configurations
            .insert(record.arn.clone(), record);
    }

    /// The receipt `client_token` already earned from `UpdateDistributionConfiguration` in
    /// `request_region`, if any.
    #[must_use]
    pub fn distribution_configuration_update_receipt(
        &self,
        request_region: &str,
        client_token: &str,
    ) -> Option<&CreateReceipt> {
        self.create_receipt(
            UPDATE_DISTRIBUTION_CONFIGURATION_OPERATION,
            request_region,
            client_token,
        )
    }
}

/// A deterministic rendering of the settings one `UpdateDistributionConfiguration` request
/// carries, so a retry of it can be told from a different request reusing its `clientToken`.
///
/// `clientToken` is left out, for the reason
/// [`distribution_configuration_request_fingerprint`] leaves it out: it is the key this
/// rendering is compared under. The ARN is part of it — the same token aimed at a different
/// configuration is a different request — and the `distributions` list is rendered by the
/// same reading a create's fingerprint renders it with, so one update and one create carrying
/// the same list render it identically.
#[must_use]
pub fn distribution_configuration_update_fingerprint(
    input: &UpdateDistributionConfigurationInput,
) -> String {
    let mut fields: BTreeMap<&'static str, String> = BTreeMap::new();
    fields.insert(
        "distributionConfigurationArn",
        text(Some(input.distribution_configuration_arn.as_str())),
    );
    fields.insert("description", text(input.description.as_deref()));
    fields.insert(
        "distributions",
        super::distributions_fingerprint(&input.distributions),
    );
    fields
        .iter()
        .map(|(member, rendered)| format!("{member}={rendered};"))
        .collect()
}
