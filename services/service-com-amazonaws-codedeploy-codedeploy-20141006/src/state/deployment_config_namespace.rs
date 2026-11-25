// SPDX-License-Identifier: CC0-1.0

//! The deployment-configuration NAME SPACE CodeDeploy keeps for the
//! configurations it ships, and the refusal a request that reaches into it earns.
//!
//! Every configuration CodeDeploy predefines is named under one prefix — the
//! shipped catalog this module's own test walks — and that prefix is the
//! service's own: a caller can never create a
//! configuration under it, whatever follows it. So the rule is about the NAME, not
//! about what the account happens to hold — a name in this space is refused by
//! `deploymentConfigName`'s own error (`InvalidDeploymentConfigNameException`)
//! before anything asks whether that name is taken, which is what lets a client
//! tell "this name is in AWS's space" from "this name of mine is already used" and
//! stops a name AWS owns from being created just because nothing shipped under it.

/// The prefix CodeDeploy reserves for the deployment configurations it
/// predefines, which every shipped configuration's name begins with.
pub const PREDEFINED_DEPLOYMENT_CONFIG_PREFIX: &str = "CodeDeployDefault.";

/// Whether `name` reaches into the reserved space — the one question a creating
/// request asks about a configuration name.
///
/// It is the PREFIX that is reserved, so a name nothing ships under
/// (`…Default.` followed by anything at all) is in the space too, while a name
/// that merely resembles the prefix without carrying it is an ordinary name.
#[must_use]
pub fn is_reserved_deployment_config_name(name: &str) -> bool {
    name.starts_with(PREDEFINED_DEPLOYMENT_CONFIG_PREFIX)
}

/// `InvalidDeploymentConfigNameException` (HTTP 400): the sentence a creating
/// request naming the reserved space earns.
///
/// The reserved PREFIX is the only thing the sentence states: the name the caller
/// sent is theirs, and what they have to change is that it starts here, so one
/// sentence serves every name in the space.
#[must_use]
pub fn reserved_deployment_config_prefix_message() -> String {
    format!(
        "The prefix {PREDEFINED_DEPLOYMENT_CONFIG_PREFIX} is reserved for predefined \
         configuration names"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped catalog lives in the reserved space: the prefix this module
    /// refuses a creating request for is the one every predefined configuration
    /// is named under, so no configuration CodeDeploy ships could be created by a
    /// caller.
    #[test]
    fn every_shipped_configuration_is_in_the_reserved_space() {
        for builtin in super::super::BUILTIN_DEPLOYMENT_CONFIGS {
            assert!(
                is_reserved_deployment_config_name(builtin.name),
                "case {:?}: every shipped configuration is named under the reserved prefix",
                builtin.name
            );
        }
    }

    /// A name outside the space is an ordinary name, however much of the prefix
    /// it carries.
    #[test]
    fn a_name_outside_the_space_is_ordinary() {
        for name in [
            "CodeDeployDefault",
            "codedeploydefault.OneAtATime",
            "my-CodeDeployDefault.OneAtATime",
            "cfg-one",
        ] {
            assert!(
                !is_reserved_deployment_config_name(name),
                "case {name:?}: expected a name outside the reserved space"
            );
        }
    }
}
