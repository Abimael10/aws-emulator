// SPDX-License-Identifier: CC0-1.0

//! Which OPTIONAL deployment capabilities one deployment pattern version supports.
//!
//! A version of a deployment pattern reports more than its name and description: it
//! reports, per capability a deployment of it may or may not have, whether that
//! capability is available — the block a client reads before it offers to show a
//! deployment's CloudFormation output, to modify an existing deployment, to run steps
//! after a deployment finished, or to move a deployment onto another version. An ABSENT
//! block is not the same answer as "no capabilities": a client that reads a capability's
//! status without a null guard faults on it, and one that reads absence as "available"
//! offers an action the version does not support.
//!
//! Each capability's status is PUBLISHED PER VERSION: it is carried on the catalogue
//! record of the version it describes ([`VersionCapabilities`] on
//! `crate::state::PatternVersion`) and reported as it stands, so one version can publish
//! a capability another does not and no status is fixed for the catalogue as a whole.
//! The one entry that is not a status — the update path the version publishes — follows
//! from that record and the SINGLE update-target rule the service applies
//! (`crate::state::DeploymentPattern::update_targets_from`), rather than from a second
//! list stored beside it.
//!
//! [`feature_flags`] is the single statement of that projection, and both version views
//! report from it (`crate::state::DeploymentPattern::summary_of`), so the page and the
//! single-version read cannot describe one version's capabilities differently.
//!
//! WHAT THIS RECORD DOES NOT DECIDE. The block is what the CATALOGUE publishes about a
//! version; it is not a gate on the operations the model declares. `UpdateDeployment`
//! keeps the rule the model gives it — `crate::state::DeploymentPattern::update_target_refusal`,
//! which the `updateFromVersion` filter of the version listing selects by as well — so a
//! move that rule allows is still applied and still reported by that filter, even for a
//! version whose record publishes no modify capability: refusing a modelled operation
//! because of a member the model declares nowhere would invent a refusal nothing states.
//! A client that gates on the block therefore offers no action this record withholds,
//! while the operations themselves answer by the rule they document.

use super::{DeploymentPattern, PatternVersion};
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

/// What ONE deployment pattern version publishes about ONE optional capability: whether
/// a deployment of that version has it, and what it is configured through.
///
/// A record on the version rather than a value computed when a version is read, so a
/// capability's status is a property of the version that publishes it: two versions of
/// one pattern can publish different states, and a status is never read off which
/// operations this emulator happens to serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionCapability {
    /// Whether a deployment of the version HAS the capability.
    pub available: bool,
    /// The fields the capability is configured through, for a capability that publishes
    /// a configuration block at all; `None` for one that publishes none. An EMPTY list
    /// is the block published with nothing to configure through, which is a different
    /// answer from publishing no block.
    pub configuration_fields: Option<Vec<String>>,
}

impl VersionCapability {
    /// A capability a deployment of the version does NOT have, publishing no
    /// configuration block of its own.
    pub fn unavailable() -> Self {
        Self {
            available: false,
            configuration_fields: None,
        }
    }

    /// A capability a deployment of the version HAS, publishing no configuration block
    /// of its own.
    pub fn available() -> Self {
        Self {
            available: true,
            configuration_fields: None,
        }
    }

    /// This capability, publishing a configuration block over `fields` — empty where the
    /// capability publishes the block and configures nothing through it.
    pub fn configured_through(self, fields: Vec<String>) -> Self {
        Self {
            configuration_fields: Some(fields),
            ..self
        }
    }

    /// This capability as a version view reports it: its status, and its configuration
    /// block where it publishes one.
    fn to_wire(&self) -> DeploymentPatternVersionCapability {
        DeploymentPatternVersionCapability {
            configuration: self.configuration_fields.clone().map(|fields| {
                DeploymentPatternVersionCapabilityConfiguration { fields }
            }),
            status: if self.available {
                CAPABILITY_ENABLED
            } else {
                CAPABILITY_DISABLED
            }
            .to_string(),
        }
    }
}

/// The optional deployment capabilities ONE version of a deployment pattern publishes:
/// one record per capability a version states a status for.
///
/// The versions an update can move a deployment onto are NOT part of this record: they
/// are the service's own update-target rule applied to the version
/// (`crate::state::DeploymentPattern::update_targets_from`), so a stored list cannot
/// contradict what an update really does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionCapabilities {
    /// Whether a deployment of the version reports the CloudFormation output of the
    /// stack it provisioned, and the fields that output is read through.
    pub cloud_formation_output: VersionCapability,
    /// Whether a deployment of the version can be modified after it was created.
    pub modify_deployments: VersionCapability,
    /// Whether a deployment of the version runs steps after the deployment itself
    /// finished.
    pub post_deployment_steps: VersionCapability,
}

/// The capability record the seeded catalogue publishes for a version of the deployment
/// patterns it holds.
///
/// The catalogue is service-owned (no API creates a workload, a pattern or a version of
/// one), so what its versions publish is seeded — like `crate::state::CATALOGUE_PATTERN_VERSIONS`
/// and the fixed catalogue-lookup wording. Launch Wizard publishes NO optional capability
/// for the deployment pattern versions this catalogue holds (recorded behaviour, which is
/// the only source for a block the model declares nowhere): a deployment of one reports no
/// CloudFormation output, is not modified after it was created and runs no step after it
/// finished. The CloudFormation-output capability still publishes its configuration block,
/// empty, because "configurable through nothing" is what an empty configuration says,
/// while the other two publish no block at all.
///
/// It is a record per version rather than a status written into the projection, so a
/// catalogue entry that publishes a capability states so on the version that publishes it.
pub fn published_capabilities() -> VersionCapabilities {
    VersionCapabilities {
        cloud_formation_output: VersionCapability::unavailable().configured_through(Vec::new()),
        modify_deployments: VersionCapability::unavailable(),
        post_deployment_steps: VersionCapability::unavailable(),
    }
}

/// The capability block `version` of `pattern` reports.
///
/// One entry per capability, every entry present:
///
/// * `cloudFormationOutput`, `modifyDeployments` and `postDeploymentSteps` are the
///   statuses the VERSION publishes, read off its own record ([`VersionCapabilities`]).
/// * `updateFromVersion` — the update path the version publishes: the versions an update
///   can move a deployment onto FROM this version, in catalogue order (newest first),
///   which is the question the `updateFromVersion` filter of
///   `ListDeploymentPatternVersions` asks of a starting version and is answered through
///   the one statement of that rule ([`DeploymentPattern::update_targets_from`]) rather
///   than from a second list beside it. A version that publishes NO modify capability
///   publishes no update path at all — moving a deployment onto another version is a
///   modification of it, so a version a deployment of which is not modified after it was
///   created names no version to move one onto — which is why this is empty wherever
///   `modifyDeployments` is.
pub fn feature_flags(
    pattern: &DeploymentPattern,
    version: &PatternVersion,
) -> DeploymentPatternVersionFeatureFlags {
    let capabilities = &version.capabilities;
    DeploymentPatternVersionFeatureFlags {
        cloud_formation_output: capabilities.cloud_formation_output.to_wire(),
        modify_deployments: capabilities.modify_deployments.to_wire(),
        post_deployment_steps: capabilities.post_deployment_steps.to_wire(),
        update_from_version: if capabilities.modify_deployments.available {
            pattern.update_targets_from(&version.version_name)
        } else {
            Vec::new()
        },
    }
}
