// SPDX-License-Identifier: CC0-1.0

//! Which of an app block builder's attributes a write may touch, and what a write it may
//! not touch is refused with.
//!
//! `UpdateAppBlockBuilder`'s own documentation states the rule: "If the app block builder
//! is in the STARTING or STOPPING state, you can't update it. If the app block builder is
//! in the RUNNING state, you can only update the DisplayName and Description. If the app
//! block builder is in the STOPPED state, you can update any attribute except the Name."
//! So the builder's own instance state decides which attributes a write may replace, and
//! what the two readings of that rule are lives here, beside the sentence a write outside
//! them is refused with — the operation reads which members the REQUEST carries and asks
//! here whether the builder's state admits them.
//!
//! Clearing an attribute lives here too: `AttributesToDelete` names the attributes of a
//! builder the modelled `AppBlockBuilderAttribute` enum declares, and which stored field
//! each of those names is stated once ([`AppBlockBuilderRecord::delete_attribute`]), so a
//! write and a read cannot disagree about what a cleared attribute leaves behind.

use super::{
    AppBlockBuilderRecord, APP_BLOCK_BUILDER_STATE_RUNNING, APP_BLOCK_BUILDER_STATE_STOPPED,
};

/// The `AppBlockBuilderAttribute` naming the IAM role applied to a builder, one of
/// `APP_BLOCK_BUILDER_ATTRIBUTE_VALUES` (`ops/types/shared_types.rs`) — the attributes
/// `UpdateAppBlockBuilder.AttributesToDelete` can remove from a builder.
pub const APP_BLOCK_BUILDER_ATTRIBUTE_IAM_ROLE_ARN: &str = "IAM_ROLE_ARN";

/// The `AppBlockBuilderAttribute` naming the interface VPC endpoints a builder is
/// administered through, another of `APP_BLOCK_BUILDER_ATTRIBUTE_VALUES`.
pub const APP_BLOCK_BUILDER_ATTRIBUTE_ACCESS_ENDPOINTS: &str = "ACCESS_ENDPOINTS";

/// The `AppBlockBuilderAttribute` naming the security groups of the builder's VPC
/// configuration, the last of `APP_BLOCK_BUILDER_ATTRIBUTE_VALUES`. It clears the security
/// groups ALONE: the subnets the builder runs in are the rest of that configuration and
/// are not what this attribute names.
pub const APP_BLOCK_BUILDER_ATTRIBUTE_VPC_CONFIGURATION_SECURITY_GROUP_IDS: &str =
    "VPC_CONFIGURATION_SECURITY_GROUP_IDS";

/// Whether `attribute` is an app block builder attribute this service's MODEL declares,
/// which is what [`AppBlockBuilderRecord::delete_attribute`] must carry a field for.
fn modelled_attribute(attribute: &str) -> bool {
    crate::ops::types::APP_BLOCK_BUILDER_ATTRIBUTE_VALUES.contains(&attribute)
}

/// Whether a builder in `state` admits a write of ANY of its attributes — the state in
/// which "you can update any attribute except the Name".
pub fn app_block_builder_accepts_any_update(state: &str) -> bool {
    state == APP_BLOCK_BUILDER_STATE_STOPPED
}

/// Whether a builder in `state` admits a write of its display members alone — the state in
/// which "you can only update the DisplayName and Description", beside the state that
/// admits every attribute. A builder on its way up or down admits neither: "if the app
/// block builder is in the STARTING or STOPPING state, you can't update it".
pub fn app_block_builder_accepts_display_update(state: &str) -> bool {
    app_block_builder_accepts_any_update(state) || state == APP_BLOCK_BUILDER_STATE_RUNNING
}

/// AWS's `ResourceInUseException` sentence for a write of attributes the builder's current
/// state does not admit.
///
/// The fixed text is AWS's; the variable parts are the builder's name, as the REQUEST
/// addressed it, and the state the stored builder is in right now — and the sentence ends
/// in a space, as AWS's own does.
pub fn app_block_builder_attributes_not_updatable_message(name: &str, state: &str) -> String {
    format!("Cannot update the attributes for AppBlockBuilder {name} when it is in {state} state. ")
}

impl AppBlockBuilderRecord {
    /// Drop the builder's value for one of the attributes the modelled
    /// `AppBlockBuilderAttribute` enum names — what
    /// `UpdateAppBlockBuilder.AttributesToDelete` asks for. An attribute outside that set
    /// names no stored value, so there is nothing to drop.
    ///
    /// A cleared attribute leaves the record without a value for it, which is what makes
    /// its member absent from the rendered `AppBlockBuilder`. Clearing never follows a
    /// reference out of this service: the IAM role and the interface VPC endpoints are
    /// other services' resources and keep their own lifecycle.
    pub fn delete_attribute(&mut self, attribute: &str) {
        match attribute {
            APP_BLOCK_BUILDER_ATTRIBUTE_IAM_ROLE_ARN => self.iam_role_arn = None,
            APP_BLOCK_BUILDER_ATTRIBUTE_ACCESS_ENDPOINTS => self.access_endpoints.clear(),
            APP_BLOCK_BUILDER_ATTRIBUTE_VPC_CONFIGURATION_SECURITY_GROUP_IDS => {
                self.vpc_config.security_group_ids = None;
            }
            other => debug_assert!(
                !modelled_attribute(other),
                "no field carries the modelled app block builder attribute {other}"
            ),
        }
    }
}
