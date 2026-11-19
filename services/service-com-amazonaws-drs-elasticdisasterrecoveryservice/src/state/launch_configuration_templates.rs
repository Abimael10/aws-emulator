// SPDX-License-Identifier: CC0-1.0

//! The Launch Configuration Template resource: what the store holds for one template,
//! how a template is reported on the wire, and how a template nothing holds is refused.
//!
//! A Launch Configuration Template is addressed by its id
//! (`LaunchConfigurationTemplateID`, `^lct-[0-9a-zA-Z]{17}$`), and its ARN is that id
//! under the resource's own ARN template
//! (`LaunchConfigurationTemplateResource`: `launch-configuration-template/{id}`), so
//! both directions live here together: [`arn`] builds a template's ARN from the id the
//! store keys it by, and the tagging route reads that id back out of an ARN
//! (`crate::state::tags`).
//!
//! The ONE place a stored template becomes a wire template
//! ([`LaunchConfigurationTemplateRecord::to_wire`]), so a created template and a
//! template read back cannot disagree about any member.

use std::collections::HashMap;

use crate::ops::types::{LaunchConfigurationTemplate, Licensing, ResourceNotFoundException};

/// The prefix of every Launch Configuration Template id: the model's
/// `LaunchConfigurationTemplateID` is `^lct-[0-9a-zA-Z]{17}$` (length 21..21), which
/// the 17-character suffix
/// [`crate::state::ComAmazonawsDrsElasticdisasterrecoveryserviceState::next_id`] mints
/// satisfies exactly.
pub const ID_PREFIX: &str = "lct";

/// The resource part of a Launch Configuration Template's ARN, as the resource's own
/// ARN template spells it (`LaunchConfigurationTemplateResource`:
/// `launch-configuration-template/{launchConfigurationTemplateID}`).
///
/// Public because the tagging route reads the KIND of resource an ARN addresses off the
/// same prefixes (`crate::state::tags`), and the two must not spell it differently.
pub const ARN_RESOURCE_PREFIX: &str = "launch-configuration-template/";

/// How this service names the Launch Configuration Template resource when it reports
/// one in an error: the `resourceType` of a not-found refusal, and the subject of its
/// message.
pub const RESOURCE_TYPE: &str = "Launch Configuration Template";

/// How many Launch Configuration Templates the store holds before
/// `CreateLaunchConfigurationTemplate` refuses with the modelled
/// `ServiceQuotaExceededException`.
///
/// This is the emulator's OWN bound on the store, not a claim about any account's AWS
/// quota (Service Quotas is not modelled here). It is set far above any workflow an
/// emulator serves, and it is what stops a caller growing the state without end.
pub const QUOTA: usize = 1_000;

/// A stored Launch Configuration Template: the members a create supplies, plus the two
/// the service assigns (`id` and `arn`).
///
/// Every setting is held as the request supplied it — `None` for a member the create
/// left out — so a read reports the template the caller described and nothing the
/// service invented for it.
#[derive(Debug, Clone, Default)]
pub struct LaunchConfigurationTemplateRecord {
    /// The `launchConfigurationTemplateID` the store keys this template by, and the
    /// resource part of [`Self::arn`].
    pub id: String,
    pub arn: String,
    /// A template always carries a tag set, so a create that named none stores an empty
    /// one: the service reports this member whether or not a client sent it.
    pub tags: HashMap<String, String>,
    pub launch_disposition: Option<String>,
    pub target_instance_type_right_sizing_method: Option<String>,
    pub copy_private_ip: Option<bool>,
    pub copy_tags: Option<bool>,
    pub licensing: Option<Licensing>,
    pub export_bucket_arn: Option<String>,
    pub post_launch_enabled: Option<bool>,
    pub launch_into_source_instance: Option<bool>,
    pub recovery_mode: Option<String>,
}

impl LaunchConfigurationTemplateRecord {
    /// The stored template as the modelled `LaunchConfigurationTemplate` structure —
    /// the output member of `CreateLaunchConfigurationTemplate` and the item shape of
    /// `DescribeLaunchConfigurationTemplates`.
    pub fn to_wire(&self) -> LaunchConfigurationTemplate {
        LaunchConfigurationTemplate {
            launch_configuration_template_id: Some(self.id.clone()),
            arn: Some(self.arn.clone()),
            tags: Some(self.tags.clone()),
            launch_disposition: self.launch_disposition.clone(),
            target_instance_type_right_sizing_method: self
                .target_instance_type_right_sizing_method
                .clone(),
            copy_private_ip: self.copy_private_ip,
            copy_tags: self.copy_tags,
            licensing: self.licensing.clone(),
            export_bucket_arn: self.export_bucket_arn.clone(),
            post_launch_enabled: self.post_launch_enabled,
            launch_into_source_instance: self.launch_into_source_instance,
            recovery_mode: self.recovery_mode.clone(),
        }
    }
}

/// The ARN of a Launch Configuration Template:
/// `arn:aws:drs:<region>:<account>:launch-configuration-template/<id>`, the resource's
/// own ARN template.
pub fn arn(region: &str, id: &str) -> String {
    ::engine::emulator_arn("drs", region, &format!("{ARN_RESOURCE_PREFIX}{id}"))
}

/// The modelled `ResourceNotFoundException` (HTTP 404) for a Launch Configuration
/// Template this store does not hold.
///
/// The ONE place that refusal is built, so every id-keyed template operation reports the
/// same `resourceType`, the same `resourceId` (the id the request named, verbatim) and
/// the same message: the service's name for the resource type, then the id it was asked
/// for, then that it was not found.
pub fn not_found(id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!("{RESOURCE_TYPE} with Id: '{id}' was not found")),
        code: None,
        resource_id: Some(id.to_string()),
        resource_type: Some(RESOURCE_TYPE.to_string()),
    }
}
