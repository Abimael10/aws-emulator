// SPDX-License-Identifier: CC0-1.0

//! The PLATFORM a placed task runs on, and the sizing it was given: the members every
//! task-returning operation reports off the placement rather than off the request that
//! read it.
//!
//! A `Task` is a record of where Amazon ECS put a workload, so the platform it resolved is
//! part of that record and is reported from the instant the task exists:
//!
//! * `Task$attributes` is "the attributes of the task", and the one Amazon ECS records for
//!   every task it places is the processor architecture its containers run on
//!   ([`ATTRIBUTE_CPU_ARCHITECTURE`]) — the architecture of the capacity it was placed on:
//!   the container instance's own reported attribute for a task that runs on one, and the
//!   architecture the revision's `runtimePlatform` names otherwise
//!   ([`requested_cpu_architecture`]);
//! * `Task$platformFamily` is "the operating system that your tasks are running on. A
//!   platform family is specified only for tasks that use the Fargate launch type" and
//!   `Task$platformVersion` "the platform version where your task runs on ... If you didn't
//!   specify one, the `LATEST` platform version is used" — so a Fargate task reports the
//!   RESOLVED version ([`resolved_platform_version`]) rather than the alias the request
//!   asked for, and a task on any other capacity reports neither;
//! * `Task$ephemeralStorage` is "the ephemeral storage settings for the task" and
//!   `Task$fargateEphemeralStorage` "the Fargate ephemeral storage settings" — the scratch
//!   space a Fargate task was given: the expansion its revision or its overrides asked for,
//!   else the amount a Fargate task gets without asking
//!   ([`FARGATE_DEFAULT_EPHEMERAL_STORAGE_GIB`]);
//! * `Task$overrides` is "one or more container overrides", which a placement always has:
//!   the task runs the containers its revision declares, so each of them is named there
//!   whether or not the request overrode anything of it ([`resolved_task_overrides`]);
//! * the size of the capacity Amazon ECS provisioned for the task, which it reports beside
//!   the task's own `cpu` and `memory` for a placement onto capacity it provisions
//!   ([`TaskRecord::provisioned_size`]).
//!
//! Resolved ONCE, by the placement, and stored on the task — not recomputed per read: a
//! later registration in the same family, or a later platform version being published,
//! does not move the platform an already-placed task runs on.

use super::*;

/// The built-in attribute naming the processor architecture of the capacity a task runs on,
/// as the container agent reports it for the instance it runs on and as
/// `Task$attributes` carries it for the task.
pub const ATTRIBUTE_CPU_ARCHITECTURE: &str = "ecs.cpu-architecture";

/// The built-in attribute naming the Availability Zone the capacity is in, reported by the
/// same agent — the zone `Task$availabilityZone` reports for a task placed on it.
pub const ATTRIBUTE_AVAILABILITY_ZONE: &str = "ecs.availability-zone";

/// The `CPUArchitecture` a revision that declares no `runtimePlatform` runs on.
/// `RuntimePlatform$cpuArchitecture`: "The CPU architecture. ... The default is `X86_64`",
/// and the enum spans `X86_64` and `ARM64`
/// (`smithy_query(query="shape",shape="CPUArchitecture")`).
pub const DEFAULT_CPU_ARCHITECTURE: &str = "X86_64";

/// The DISPLAY spelling of a `CPUArchitecture` value, which is what the
/// [`ATTRIBUTE_CPU_ARCHITECTURE`] attribute carries: the enum's own name in lower case
/// (`X86_64` -> `x86_64`, `ARM64` -> `arm64`), the spelling the container agent reports an
/// instance's architecture under.
pub fn cpu_architecture_display(cpu_architecture: &str) -> String {
    cpu_architecture.to_lowercase()
}

/// The CPU architecture a stored or requested `runtimePlatform` names: the one it declares,
/// else the default a revision that declares none runs on ([`DEFAULT_CPU_ARCHITECTURE`]).
pub fn requested_cpu_architecture(runtime_platform: Option<&RuntimePlatform>) -> String {
    runtime_platform
        .and_then(|platform| platform.cpu_architecture.clone())
        .unwrap_or_else(|| DEFAULT_CPU_ARCHITECTURE.to_string())
}

/// The ephemeral storage, in GiB, a Fargate task gets when nothing asks for more.
///
/// `TaskEphemeralStorage$sizeInGiB`: "The total amount, in GiB, of the ephemeral storage to
/// set for the task. The minimum supported value is `20` GiB" — and `EphemeralStorage` is
/// what "expand[s] the total amount of ephemeral storage available, beyond the default
/// amount", so the amount a task that expands nothing holds is that minimum.
pub const FARGATE_DEFAULT_EPHEMERAL_STORAGE_GIB: i32 = 20;

/// The ephemeral storage, in GiB, a Fargate placement gives the task: the expansion its
/// `overrides` asked for ("The ephemeral storage setting override for the task"), else the
/// one its revision registered, else [`FARGATE_DEFAULT_EPHEMERAL_STORAGE_GIB`].
pub fn resolved_ephemeral_storage_gib(
    registered: Option<&EphemeralStorage>,
    overrides: Option<&TaskOverride>,
) -> i32 {
    overrides
        .and_then(|overrides| overrides.ephemeral_storage.as_ref())
        .or(registered)
        .map_or(FARGATE_DEFAULT_EPHEMERAL_STORAGE_GIB, |storage| {
            storage.size_in_gi_b
        })
}

/// The `TaskOverride` a placement runs under: what the request overrode, with one
/// `containerOverrides` entry for every container of the revision it places.
///
/// `TaskOverride$containerOverrides` is "one or more container overrides that are sent to a
/// task" and a `ContainerOverride` is "the name of a container in the specified task
/// definition and the overrides it should receive", so every container of the task has an
/// entry: the one the request carried for it, else the container's name alone, which
/// overrides nothing. The order is the revision's own container order, so the list a caller
/// reads back is in the order the containers are declared in rather than the order a
/// request happened to name a few of them.
///
/// `inferenceAcceleratorOverrides` is materialised as the EMPTY LIST a placement that asked
/// for none carries, so a client may index it unconditionally — "Amazon Elastic Inference
/// (EI) is no longer available to customers" (`RunTask`), so nothing here mints one.
pub fn resolved_task_overrides(
    requested: Option<&TaskOverride>,
    container_definitions: &[ContainerDefinition],
) -> TaskOverride {
    let requested_containers = requested
        .and_then(|overrides| overrides.container_overrides.as_ref())
        .map(Vec::as_slice)
        .unwrap_or_default();
    let container_overrides = container_definitions
        .iter()
        .map(|definition| {
            requested_containers
                .iter()
                .find(|override_| {
                    override_.name.is_some() && override_.name == definition.name
                })
                .cloned()
                .unwrap_or_else(|| ContainerOverride {
                    name: definition.name.clone(),
                    ..ContainerOverride::default()
                })
        })
        .collect();
    TaskOverride {
        container_overrides: Some(container_overrides),
        inference_accelerator_overrides: Some(
            requested
                .and_then(|overrides| overrides.inference_accelerator_overrides.clone())
                .unwrap_or_default(),
        ),
        cpu: requested.and_then(|overrides| overrides.cpu.clone()),
        memory: requested.and_then(|overrides| overrides.memory.clone()),
        task_role_arn: requested.and_then(|overrides| overrides.task_role_arn.clone()),
        execution_role_arn: requested.and_then(|overrides| overrides.execution_role_arn.clone()),
        ephemeral_storage: requested.and_then(|overrides| overrides.ephemeral_storage.clone()),
    }
}

/// The `capacityProviderStrategy` a placement made THROUGH one capacity provider is read
/// as: the one-entry strategy naming it, and an empty one for a placement that named none.
///
/// What lets every Fargate question about a placement go through the one predicate
/// ([`places_on_fargate`]), whether the placement carries a whole strategy or just the
/// provider it resolved to.
pub fn placed_through_strategy(
    capacity_provider_name: Option<&str>,
) -> Vec<CapacityProviderStrategyItem> {
    capacity_provider_name
        .map(|name| {
            vec![CapacityProviderStrategyItem {
                capacity_provider: name.to_string(),
                ..CapacityProviderStrategyItem::default()
            }]
        })
        .unwrap_or_default()
}

/// Everything about the PLATFORM one placement resolved, which the task it places carries
/// for the rest of its life. See the module docs for the member each field is reported as.
#[derive(Debug, Clone)]
pub struct TaskPlatform {
    /// `Task$platformFamily`, and `None` for a task that runs on no managed platform.
    pub platform_family: Option<String>,
    /// `Task$platformVersion` — the RESOLVED version — and `None` for the same tasks.
    pub platform_version: Option<String>,
    /// The ephemeral storage the task was given, in GiB, and `None` for a task whose
    /// capacity reports none.
    pub ephemeral_storage_gib: Option<i32>,
}

/// The platform a placement of `runtime_platform` resolves, given whether it places the
/// task on Fargate ([`places_on_fargate`]) and the platform version it asked for.
///
/// The Fargate-only members are resolved exactly for a Fargate placement, because that is
/// what their own docs scope them to ("A platform family is specified only for tasks that
/// use the Fargate launch type"). The architecture every task's containers run on is
/// resolved beside this, against the capacity the placement chose
/// ([`task_cpu_architecture`]).
pub fn resolved_task_platform(
    runtime_platform: Option<&RuntimePlatform>,
    registered_storage: Option<&EphemeralStorage>,
    overrides: Option<&TaskOverride>,
    fargate: bool,
    requested_platform_version: Option<&str>,
) -> TaskPlatform {
    let os_family = requested_os_family(runtime_platform);
    TaskPlatform {
        platform_family: fargate.then(|| os_family_display(&os_family)),
        platform_version: fargate.then(|| {
            resolved_platform_version(
                requested_platform_version.unwrap_or(PLATFORM_VERSION_LATEST),
                &os_family,
            )
        }),
        ephemeral_storage_gib: fargate
            .then(|| resolved_ephemeral_storage_gib(registered_storage, overrides)),
    }
}

/// The `CPUArchitecture` a task's containers run on: the architecture of the CAPACITY the
/// placement put it on.
///
/// `host_reported` is what the capacity reports for itself — the
/// [`ATTRIBUTE_CPU_ARCHITECTURE`] attribute of the container instance the task runs on
/// ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::host_cpu_architecture`]), and
/// `None` for a task that runs on none (a Fargate task) or on one whose agent reported no
/// architecture. Without it the architecture is the one the revision's `runtimePlatform`
/// names, defaulted as that member documents ([`requested_cpu_architecture`]): that is what
/// Amazon ECS provisions Fargate capacity for, and it is the best this emulator holds about
/// a host that reported nothing.
pub fn task_cpu_architecture(
    host_reported: Option<&str>,
    runtime_platform: Option<&RuntimePlatform>,
) -> String {
    match host_reported {
        Some(reported) => reported.to_string(),
        None => requested_cpu_architecture(runtime_platform),
    }
}

impl TaskRecord {
    /// The size of the capacity Amazon ECS PROVISIONED for this task — `(cpu, memory)`, in
    /// the units the task's own `cpu` and `memory` count in — and `None` for a task whose
    /// capacity Amazon ECS does not provision.
    ///
    /// A task placed on Fargate runs on infrastructure Amazon Web Services provisions FOR
    /// that task, sized by the task itself ("This field is required for Fargate",
    /// `Task$cpu`), so what was provisioned for it is the task's resolved size; a task
    /// placed on capacity registered to the cluster runs on an instance the account
    /// provisioned for itself, whose size is no part of the task, so the task reports no
    /// provisioned size at all.
    ///
    /// Read off the task's own reported size, so the provisioned size and the size the
    /// task reports can never disagree.
    pub fn provisioned_size(&self) -> Option<(String, String)> {
        if !self.runs_on_fargate() {
            return None;
        }
        let cpu = self.cpu.clone()?;
        let memory = self.memory.clone()?;
        Some((cpu, memory))
    }
}
