// SPDX-License-Identifier: CC0-1.0

//! Which OPTIONAL deployment capabilities one deployment pattern version supports.
//!
//! A version of a deployment pattern reports more than its name and description: it
//! reports, per capability a deployment of it may or may not have, whether that
//! capability is available — the block a client reads before it offers to show a
//! deployment's CloudFormation output, to modify an existing deployment, to run steps
//! after a deployment finished, or to move a deployment onto this version. An ABSENT
//! block is not the same answer as "no capabilities": a client that reads a capability's
//! status without a null guard faults on it, and one that reads absence as "available"
//! offers an action the version does not support.
//!
//! Each status here is read off WHAT THIS EMULATOR'S DEPLOYMENT PLANE ACTUALLY SERVES for
//! a deployment of the version, never off a seeded value beside it, so a capability
//! reported as available is one a client can then exercise and one reported as
//! unavailable is one nothing here serves. [`feature_flags`] is the single statement of
//! that projection, and both version views report from it
//! (`crate::state::DeploymentPattern::summary_of`), so the page and the single-version
//! read cannot describe one version's capabilities differently.

use super::DeploymentPattern;
use crate::ops::types::{
    DeploymentPatternVersionCapability, DeploymentPatternVersionCapabilityConfiguration,
    DeploymentPatternVersionFeatureFlags,
};

/// The status of a capability a deployment of the version HAS.
///
/// The two values a capability's status takes, as the recorded capability block carries
/// them. Written out here because the model declares the block nowhere, so there is no
/// enum to read them off (see `crate::ops::types::deployment_pattern_version_wire`).
pub const CAPABILITY_ENABLED: &str = "ENABLED";
/// The status of a capability a deployment of the version does NOT have. See
/// [`CAPABILITY_ENABLED`].
pub const CAPABILITY_DISABLED: &str = "DISABLED";

/// One capability that is unavailable and carries no configuration of its own.
fn unavailable() -> DeploymentPatternVersionCapability {
    DeploymentPatternVersionCapability {
        configuration: None,
        status: CAPABILITY_DISABLED.to_string(),
    }
}

/// One capability that is available and carries no configuration of its own.
fn available() -> DeploymentPatternVersionCapability {
    DeploymentPatternVersionCapability {
        configuration: None,
        status: CAPABILITY_ENABLED.to_string(),
    }
}

/// The capability block the version named `version_name` of `pattern` reports.
///
/// One entry per capability, every entry present, each one answered from what this
/// emulator serves for a deployment of that version:
///
/// * `cloudFormationOutput` — a deployment's reads report the resource group the
///   deployment's own resources are collected in (`DeploymentData$resourceGroup`) and no
///   stack output: no operation of this service reports one, and nothing stores one, so
///   the capability is unavailable and its configuration names no field to read an output
///   through. The configuration is still REPORTED, empty, because "configurable through
///   nothing" is what an empty configuration says.
/// * `modifyDeployments` — `UpdateDeployment` applies new settings to a deployment that
///   already exists, and moves it onto another version of its pattern, so a deployment of
///   this version can be modified after it was created.
/// * `postDeploymentSteps` — the deployment plane appends the lifecycle events a
///   deployment walks and runs no step of its own after a deployment finished, so the
///   capability is unavailable.
/// * `updateFromVersion` — the versions an update can move a deployment onto this version
///   FROM, in catalogue order (newest first). Read through
///   [`DeploymentPattern::is_update_target`], the ONE statement of the update-target rule
///   `UpdateDeployment` applies and `ListDeploymentPatternVersions`' `updateFromVersion`
///   filter selects by, so the capability block cannot name a move the update refuses nor
///   leave out one it allows. Empty for a version no other published version can be
///   updated to — which is every version of a pattern that publishes only one.
pub fn feature_flags(
    pattern: &DeploymentPattern,
    version_name: &str,
) -> DeploymentPatternVersionFeatureFlags {
    DeploymentPatternVersionFeatureFlags {
        cloud_formation_output: DeploymentPatternVersionCapability {
            configuration: Some(DeploymentPatternVersionCapabilityConfiguration {
                fields: Vec::new(),
            }),
            ..unavailable()
        },
        modify_deployments: available(),
        post_deployment_steps: unavailable(),
        update_from_version: pattern
            .versions
            .iter()
            .filter(|candidate| pattern.is_update_target(&candidate.version_name, version_name))
            .map(|candidate| candidate.version_name.clone())
            .collect(),
    }
}
