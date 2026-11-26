// SPDX-License-Identifier: CC0-1.0

//! The `linuxParameters` ONE container definition declares, as the rules about its values
//! read it.
//!
//! `LinuxParameters` is "The Linux-specific options that are applied to the container", and
//! two of its members carry values Amazon ECS publishes a closed catalog for that the model
//! cannot hold — the kernel capabilities ([`super::kernel_capabilities`]) and a tmpfs mount's
//! options ([`super::tmpfs_mount_options`]). Both rules are about one container's own members
//! and are checked for every operation that registers container definitions
//! ([`crate::ops::register_task_definition`] and
//! [`crate::ops::register_daemon_task_definition`]), whose `linuxParameters` shapes differ
//! while the members these rules read are the same — which is why they are read through the
//! borrowed view here rather than off either shape. A registration that breaks one of them is
//! refused BEFORE any revision is allocated, so the family's revision counter does not advance
//! for a request Amazon ECS never persists.

use crate::ops::types::{DaemonLinuxParameters, KernelCapabilities, LinuxParameters, Tmpfs};

/// What one container definition declares under `linuxParameters` for these rules, read off
/// whichever shape the registration carries.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeclaredLinuxParameters<'a> {
    /// `LinuxParameters$capabilities` — the capabilities added to and dropped from the set.
    pub capabilities: Option<&'a KernelCapabilities>,
    /// `LinuxParameters$tmpfs` — the tmpfs volumes mounted into the container.
    pub tmpfs: &'a [Tmpfs],
}

impl<'a> DeclaredLinuxParameters<'a> {
    /// Read the `linuxParameters` of one `ContainerDefinition`, which may declare none.
    pub fn of_linux_parameters(parameters: Option<&'a LinuxParameters>) -> Self {
        match parameters {
            Some(parameters) => Self {
                capabilities: parameters.capabilities.as_ref(),
                tmpfs: parameters.tmpfs.as_deref().unwrap_or_default(),
            },
            None => Self::default(),
        }
    }

    /// The same for a `DaemonContainerDefinition`, whose own shape declares the two members.
    pub fn of_daemon_linux_parameters(parameters: Option<&'a DaemonLinuxParameters>) -> Self {
        match parameters {
            Some(parameters) => Self {
                capabilities: parameters.capabilities.as_ref(),
                tmpfs: parameters.tmpfs.as_deref().unwrap_or_default(),
            },
            None => Self::default(),
        }
    }
}

/// The first value `declared` carries that is outside the catalog its own member publishes,
/// stated as the refusal's message — or `None` when every value in it names something.
///
/// The members are weighed in the order the shape declares them: the capabilities
/// ([`super::kernel_capabilities_violation`]) before the tmpfs mounts
/// ([`super::tmpfs_mount_option_violation`]), so the refusal a container earns never depends
/// on anything but its values.
pub fn linux_parameters_violation(declared: DeclaredLinuxParameters<'_>) -> Option<String> {
    super::kernel_capabilities_violation(declared.capabilities)
        .or_else(|| super::tmpfs_mount_option_violation(declared.tmpfs))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parameters a container declares when it names `add` capabilities and one tmpfs
    /// mount with `options`.
    fn declaring(add: &[&str], options: &[&str]) -> LinuxParameters {
        LinuxParameters {
            capabilities: Some(KernelCapabilities {
                add: Some(add.iter().map(|name| (*name).to_string()).collect()),
                drop: None,
            }),
            devices: None,
            init_process_enabled: None,
            shared_memory_size: None,
            tmpfs: Some(vec![Tmpfs {
                container_path: "/run".to_string(),
                size: 64,
                mount_options: Some(options.iter().map(|option| (*option).to_string()).collect()),
            }]),
            max_swap: None,
            swappiness: None,
        }
    }

    /// One row of the table below: the case, the capabilities the container adds, the mount
    /// options it declares, and the sentence it earns.
    type Row<'a> = (&'a str, &'a [&'a str], &'a [&'a str], Option<&'a str>);

    /// RULE: both members are weighed, and the capabilities — which the shape declares first —
    /// are what a container that gets both wrong is refused for.
    #[test]
    fn both_members_are_weighed_in_the_order_the_shape_declares_them() {
        let rows: &[Row<'_>] = &[
            ("both in their catalogs", &["SYS_PTRACE"], &["noexec"], None),
            (
                "only the mount option outside its catalog",
                &["SYS_PTRACE"],
                &["NOEXEC"],
                Some("The NOEXEC tmpfs mount option specified is invalid."),
            ),
            (
                "only the capability outside its catalog",
                &["CAP_SYS_PTRACE"],
                &["noexec"],
                Some("Unrecognized Linux capabilities in add: [CAP_SYS_PTRACE]"),
            ),
            (
                "both outside their catalogs",
                &["CAP_SYS_PTRACE"],
                &["NOEXEC"],
                Some("Unrecognized Linux capabilities in add: [CAP_SYS_PTRACE]"),
            ),
        ];
        for (case, add, options, message) in rows {
            let parameters = declaring(add, options);
            let answer = linux_parameters_violation(
                DeclaredLinuxParameters::of_linux_parameters(Some(&parameters)),
            );
            assert_eq!(
                answer.as_deref(),
                *message,
                "case {case:?}: expected {message:?}, got {answer:?}"
            );
        }
    }

    /// RULE: a container that declares no `linuxParameters` at all breaks neither rule,
    /// whichever of the two shapes it came in on.
    #[test]
    fn a_container_that_declares_none_is_accepted() {
        for (case, declared) in [
            (
                "no linuxParameters",
                DeclaredLinuxParameters::of_linux_parameters(None),
            ),
            (
                "no daemon linuxParameters",
                DeclaredLinuxParameters::of_daemon_linux_parameters(None),
            ),
            (
                "daemon parameters declaring neither member",
                DeclaredLinuxParameters::of_daemon_linux_parameters(Some(
                    &DaemonLinuxParameters {
                        capabilities: None,
                        devices: None,
                        init_process_enabled: None,
                        tmpfs: None,
                    },
                )),
            ),
        ] {
            let answer = linux_parameters_violation(declared);
            assert!(
                answer.is_none(),
                "case {case:?}: expected the container to be accepted, got {answer:?}"
            );
        }
    }
}
