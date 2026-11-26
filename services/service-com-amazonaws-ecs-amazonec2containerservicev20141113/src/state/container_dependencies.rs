// SPDX-License-Identifier: CC0-1.0

//! The STARTUP DEPENDENCIES a registration's container definitions declare: the condition one
//! container waits for another on, and the condition that can only be met by a container that
//! is probed.
//!
//! `ContainerDependency$condition` documents what each condition waits for, and one of them
//! names a requirement on the container being waited FOR: "`HEALTHY` - This condition
//! validates that the dependent container passes its Docker health check before permitting
//! other containers to start. This requires that the dependent container has health checks
//! configured." A container with no `healthCheck` is never probed, so it can never pass one —
//! a task whose startup waits on that could never start at all, which is why the registration
//! is refused rather than stored.
//!
//! The rule is about the container list as a WHOLE (the dependency names another container of
//! the same revision) and is not expressible in the model, so it is checked here, once, for
//! every operation that registers container definitions
//! ([`crate::ops::register_task_definition`] and
//! [`crate::ops::register_daemon_task_definition`]) — which is why it reads a borrowed
//! [`ContainerStartupDependencies`] view rather than one of the two container shapes this API
//! declares. A registration that breaks it is refused BEFORE any revision is allocated, so
//! the family's revision counter does not advance for a request Amazon ECS never persists.

use crate::ops::types::{ContainerDefinition, ContainerDependency, DaemonContainerDefinition};

// The condition this rule is about, spelled once for this crate beside the readings the
// scheduler weighs a dependency by ([`super::container_health`]).
use super::CONTAINER_CONDITION_HEALTHY;

/// What one container definition says about the containers its start waits for, read off
/// whichever container shape the registration carries (`ContainerDefinition` or
/// `DaemonContainerDefinition`, whose members for this rule are the same).
#[derive(Debug, Clone, Copy)]
pub struct ContainerStartupDependencies<'a> {
    /// `ContainerDefinition$name` — how the dependencies of the other containers name this
    /// one.
    pub name: &'a str,
    /// Whether this container is PROBED: it declares a `healthCheck` ("The container health
    /// check command and associated configuration parameters for the container").
    pub declares_health_check: bool,
    /// `ContainerDefinition$dependsOn` — the containers this one's start waits for.
    pub depends_on: &'a [ContainerDependency],
}

impl<'a> ContainerStartupDependencies<'a> {
    /// Read one `ContainerDefinition` of a task-definition registration.
    pub fn of_container(definition: &'a ContainerDefinition) -> Self {
        Self {
            name: definition.name.as_deref().unwrap_or_default(),
            declares_health_check: definition.health_check.is_some(),
            depends_on: definition.depends_on.as_deref().unwrap_or_default(),
        }
    }

    /// Read one `DaemonContainerDefinition` of a daemon registration: the same members,
    /// declared on that resource's own container shape.
    pub fn of_daemon_container(definition: &'a DaemonContainerDefinition) -> Self {
        Self {
            name: definition.name.as_deref().unwrap_or_default(),
            declares_health_check: definition.health_check.is_some(),
            depends_on: definition.depends_on.as_deref().unwrap_or_default(),
        }
    }
}

/// `ClientException$message` for a registration in which a container's start waits for
/// another container to be HEALTHY while that container declares no health check.
///
/// ONE fixed sentence: it names neither of the two containers and neither the condition's
/// position in the list nor the member path it came in on. RECORDED behaviour, with no
/// trailing period.
pub const HEALTHY_DEPENDENCY_NEEDS_HEALTH_CHECK_MESSAGE: &str =
    "A dependency container with HEALTHY condition must have health check configured";

/// The FIRST dependency of `containers` that waits for a container that is never probed,
/// stated as the refusal's message — or `None` when every `HEALTHY` condition names a
/// container of the list that declares a health check.
///
/// The containers are walked in the order the request lists them, and each one's
/// dependencies in the order it declares them, so the refusal a list earns never depends on
/// anything but its values.
///
/// A condition naming a container the list does NOT describe is left alone: the dependency
/// names no container of this revision at all, which is a different condition from one whose
/// container is not probed, and Amazon ECS states this sentence for the health check.
pub fn container_dependency_violation(
    containers: &[ContainerStartupDependencies<'_>],
) -> Option<String> {
    for container in containers {
        for dependency in container.depends_on {
            if dependency.condition != CONTAINER_CONDITION_HEALTHY {
                continue;
            }
            let waits_for_unprobed = containers
                .iter()
                .find(|candidate| candidate.name == dependency.container_name)
                .is_some_and(|candidate| !candidate.declares_health_check);
            if waits_for_unprobed {
                return Some(HEALTHY_DEPENDENCY_NEEDS_HEALTH_CHECK_MESSAGE.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A container that waits for nothing and is not probed: the base every row below
    /// changes.
    fn plain(name: &str) -> ContainerStartupDependencies<'_> {
        ContainerStartupDependencies {
            name,
            declares_health_check: false,
            depends_on: &[],
        }
    }

    /// One dependency on `container_name` under `condition`.
    fn waits_for(container_name: &str, condition: &str) -> ContainerDependency {
        ContainerDependency {
            container_name: container_name.to_string(),
            condition: condition.to_string(),
        }
    }

    /// RULE: a `HEALTHY` condition needs the container it names to declare a health check;
    /// every other condition needs nothing of it.
    #[test]
    fn only_the_healthy_condition_requires_a_probe() {
        // (the condition the dependency carries, whether an unprobed dependency is refused)
        let rows: &[(&str, bool)] = &[
            ("START", false),
            ("COMPLETE", false),
            ("SUCCESS", false),
            (CONTAINER_CONDITION_HEALTHY, true),
        ];
        for (condition, refused) in rows {
            let dependencies = [waits_for("sidecar", condition)];
            let mut app = plain("app");
            app.depends_on = &dependencies;
            let unprobed = container_dependency_violation(&[plain("sidecar"), app]);
            assert_eq!(
                unprobed.is_some(),
                *refused,
                "case {condition:?}: expected refused == {refused}, got {unprobed:?}"
            );
            if *refused {
                assert_eq!(
                    unprobed.as_deref(),
                    Some(HEALTHY_DEPENDENCY_NEEDS_HEALTH_CHECK_MESSAGE),
                    "case {condition:?}: expected the fixed sentence, got {unprobed:?}"
                );
            }
            let mut probed = plain("sidecar");
            probed.declares_health_check = true;
            let beside = container_dependency_violation(&[probed, app]);
            assert!(
                beside.is_none(),
                "case {condition:?}: a probed dependency satisfies every condition, got \
                 {beside:?}"
            );
        }
    }

    /// RULE: the dependency is resolved against the list the request carries, whatever order
    /// the two containers come in, and a name the list does not describe is not this
    /// condition.
    #[test]
    fn the_dependency_is_resolved_against_the_list() {
        let dependencies = [waits_for("sidecar", CONTAINER_CONDITION_HEALTHY)];
        let mut app = plain("app");
        app.depends_on = &dependencies;
        for (case, list) in [
            ("the dependency first", vec![plain("sidecar"), app]),
            ("the dependency last", vec![app, plain("sidecar")]),
        ] {
            let answer = container_dependency_violation(&list);
            assert_eq!(
                answer.as_deref(),
                Some(HEALTHY_DEPENDENCY_NEEDS_HEALTH_CHECK_MESSAGE),
                "case {case:?}: expected the fixed sentence, got {answer:?}"
            );
        }
        let elsewhere = container_dependency_violation(&[app]);
        assert!(
            elsewhere.is_none(),
            "case \"a container the list does not describe\": expected no refusal of this \
             rule, got {elsewhere:?}"
        );
    }
}
