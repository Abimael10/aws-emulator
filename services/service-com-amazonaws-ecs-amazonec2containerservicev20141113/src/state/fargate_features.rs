// SPDX-License-Identifier: CC0-1.0

//! The container-definition and volume features FARGATE capacity provides nothing for.
//!
//! A Fargate task runs on capacity Amazon ECS owns, so a revision may only declare what
//! that capacity can present to a container. Two members name something the CALLER's own
//! container instance would have to supply, and each member's own documentation says so:
//!
//! * `ResourceRequirement$type` `GPU` is "the number of physical `GPUs` the Amazon ECS
//!   container agent reserves for the container", bounded by "the number of available GPUs
//!   on the container instance that the task is launched on";
//! * `HostVolumeProperties$sourcePath` is "the path on the host container instance that's
//!   presented to the container", and the member adds "If you're using the Fargate launch
//!   type, the `sourcePath` parameter is not supported."
//!
//! So the rule is read BOTH ways from here, as one statement of it: a registration that
//! REQUIRES Fargate and declares one of these is refused before any revision exists
//! ([`fargate_feature_violation`]), and a stored revision that declares one reports no
//! Fargate compatibility ([`TaskDefinitionRecord::needs_capacity_fargate_lacks`]), so the
//! launch types a revision admits and the registrations Amazon ECS accepts cannot
//! disagree.

use super::*;

/// The `ResourceType` naming a physical device the container instance attaches
/// (`smithy_query(query="shape",shape="ResourceType")`: `GPU`, `InferenceAccelerator`,
/// `NeuronDevice`), of which `GPU` is the one Amazon ECS words a Fargate refusal for.
const RESOURCE_TYPE_GPU: &str = "GPU";

/// `ClientException$message` for a Fargate registration whose containers reserve a GPU.
/// One fixed sentence: the member is the same on every container, so nothing of the
/// request narrows it.
pub const FARGATE_GPU_UNSUPPORTED_MESSAGE: &str =
    "Tasks using the Fargate launch type do not support GPU resource requirements.";

/// `ClientException$message` for a Fargate registration that bind-mounts a path of a
/// container instance, which Amazon ECS states by naming the one member that asked for it.
pub const FARGATE_SOURCE_PATH_UNSUPPORTED_MESSAGE: &str =
    "Fargate compatible task definitions do not support sourcePath";

/// Whether any container of the list reserves a GPU — `resourceRequirements` of type
/// [`RESOURCE_TYPE_GPU`], whatever count it asks for.
pub fn reserves_a_gpu(containers: &[ContainerDefinition]) -> bool {
    containers.iter().any(|container| {
        container
            .resource_requirements
            .iter()
            .flatten()
            .any(|requirement| requirement.r#type == RESOURCE_TYPE_GPU)
    })
}

/// Whether any volume of the list bind-mounts a path of the container instance — a `host`
/// volume that names a `sourcePath`.
///
/// A `host` volume that names NONE is a different volume: "If this parameter is empty,
/// then the Docker daemon has assigned a host path for you", which asks for no path of the
/// instance, so it is not what the refusal is about.
pub fn binds_a_host_path(volumes: &[Volume]) -> bool {
    volumes.iter().any(|volume| {
        volume.host.as_ref().is_some_and(|host| {
            host.source_path
                .as_deref()
                .is_some_and(|path| !path.trim().is_empty())
        })
    })
}

/// The refusal a FARGATE registration earns for declaring a feature Fargate provides
/// nothing for, or `None` when it declares none.
///
/// The members are weighed in the order the operation declares them,
/// `containerDefinitions` before `volumes`, so the refusal a request earns never depends
/// on anything but its values.
pub fn fargate_feature_violation(
    containers: &[ContainerDefinition],
    volumes: &[Volume],
) -> Option<&'static str> {
    if reserves_a_gpu(containers) {
        return Some(FARGATE_GPU_UNSUPPORTED_MESSAGE);
    }
    if binds_a_host_path(volumes) {
        return Some(FARGATE_SOURCE_PATH_UNSUPPORTED_MESSAGE);
    }
    None
}

impl TaskDefinitionRecord {
    /// Whether this stored revision declares a feature Fargate provides nothing for, so
    /// Fargate is a launch type it was never validated against
    /// ([`TaskDefinitionRecord::compatibilities`]) — the same rule the registration above
    /// is refused by, read off the record.
    pub fn needs_capacity_fargate_lacks(&self) -> bool {
        fargate_feature_violation(&self.container_definitions, &self.volumes).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::{HostVolumeProperties, ResourceRequirement};

    /// A container that declares nothing but its name, the base each row below adds one
    /// member to.
    fn plain(name: &str) -> ContainerDefinition {
        ContainerDefinition {
            name: Some(name.to_string()),
            ..ContainerDefinition::default()
        }
    }

    /// A container reserving `count` of resource `kind`.
    fn reserving(kind: &str, count: &str) -> ContainerDefinition {
        ContainerDefinition {
            resource_requirements: Some(vec![ResourceRequirement {
                r#type: kind.to_string(),
                value: count.to_string(),
            }]),
            ..plain("app")
        }
    }

    /// RULE: a Fargate registration is refused for a GPU reservation whatever count it
    /// asks for and whichever container of the list carries it, and for nothing else in
    /// the member.
    #[test]
    fn a_gpu_reservation_is_refused_on_fargate() {
        // (case, the container list, refused)
        let rows: &[(&str, Vec<ContainerDefinition>, bool)] = &[
            ("no resource requirements", vec![plain("app")], false),
            ("one GPU", vec![reserving("GPU", "1")], true),
            ("every GPU on the instance", vec![reserving("GPU", "ALL")], true),
            ("several GPUs", vec![reserving("GPU", "4")], true),
            (
                "a GPU on the second container",
                vec![plain("app"), reserving("GPU", "1")],
                true,
            ),
            // The other `ResourceType` values name their own members and their own
            // documented rules, so this sentence is not theirs.
            (
                "an inference accelerator",
                vec![reserving("InferenceAccelerator", "device1")],
                false,
            ),
            ("a Neuron device", vec![reserving("NeuronDevice", "ALL")], false),
        ];
        for (case, containers, refused) in rows {
            let answer = fargate_feature_violation(containers, &[]);
            assert_eq!(
                answer,
                refused.then_some(FARGATE_GPU_UNSUPPORTED_MESSAGE),
                "case {case:?}: expected refused == {refused}, got {answer:?}"
            );
        }
    }

    /// RULE: a Fargate registration is refused for a `host` volume that names a
    /// `sourcePath`, and accepted for every other volume shape.
    #[test]
    fn a_host_source_path_is_refused_on_fargate() {
        /// A volume whose `host` carries `source_path`, when it carries a `host` at all.
        fn volume(name: &str, host: Option<Option<&str>>) -> Volume {
            Volume {
                name: Some(name.to_string()),
                host: host.map(|source_path| HostVolumeProperties {
                    source_path: source_path.map(str::to_string),
                }),
                ..Volume::default()
            }
        }

        // (case, the volume list, refused)
        let rows: &[(&str, Vec<Volume>, bool)] = &[
            ("no volumes", vec![], false),
            ("a volume with no host", vec![volume("scratch", None)], false),
            (
                "an empty host volume",
                vec![volume("scratch", Some(None))],
                false,
            ),
            (
                "a host volume naming a path",
                vec![volume("state", Some(Some("/var/lib/app")))],
                true,
            ),
            (
                "a host volume naming a Windows path",
                vec![volume("state", Some(Some("C:\\ProgramData\\app")))],
                true,
            ),
            (
                "a path on the second volume",
                vec![
                    volume("scratch", Some(None)),
                    volume("state", Some(Some("/data"))),
                ],
                true,
            ),
        ];
        for (case, volumes, refused) in rows {
            let answer = fargate_feature_violation(&[plain("app")], volumes);
            assert_eq!(
                answer,
                refused.then_some(FARGATE_SOURCE_PATH_UNSUPPORTED_MESSAGE),
                "case {case:?}: expected refused == {refused}, got {answer:?}"
            );
        }
    }
}
