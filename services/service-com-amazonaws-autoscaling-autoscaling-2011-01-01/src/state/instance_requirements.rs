// SPDX-License-Identifier: CC0-1.0

//! The instance type a group's ATTRIBUTE-BASED launch source launches, and the unit a
//! group's capacity is counted in.
//!
//! A mixed instances policy states the types to launch in one of two ways: an override
//! names an `InstanceType` outright, or it states `InstanceRequirements` and "Amazon EC2
//! Auto Scaling will select instance types with the provided requirements"
//! (`smithy_query(query="doc",shape="LaunchTemplateOverrides",member="InstanceRequirements")`).
//! The second way is what `DesiredCapacityType` exists for: "the unit of measurement for
//! the value specified for desired capacity … Amazon EC2 Auto Scaling supports
//! `DesiredCapacityType` for attribute-based instance type selection only", with "`units`,
//! which translates into number of instances" by default and `vcpu` or `memory-mib` naming
//! the instance attribute the capacity is counted in instead
//! (`…member="DesiredCapacityType"`).
//!
//! So this module answers two questions about one group, from ONE reading of its
//! requirements:
//!
//! * WHICH type a launch of it produces ([`selected_instance_type`]), chosen from the
//!   instance types the account's own Amazon EC2 catalog publishes
//!   ([`ec2::offered_instance_types`]) — the service that owns instance types and their
//!   attributes — rather than from a list written out here;
//! * how many units ONE instance of a type provides ([`capacity_units`]), which is what a
//!   group counted in `vcpu` fulfils its `DesiredCapacity` with: a group asking for four
//!   vCPUs of two-vCPU instances holds two of them, not four.
//!
//! The attributes a requirement is taken against are the ones Amazon EC2 PUBLISHES for a
//! type ([`ec2::InstanceTypeRecord`]): its vCPUs, its memory, whether it is bare metal,
//! burstable or the current generation of its family, its processor architecture and
//! whether it supports hibernation. A requirement about something that catalog does not
//! publish (an accelerator's make, a baseline EBS bandwidth) narrows nothing here, rather
//! than being answered from a figure invented for it.
//!
//! # What the selection costs, and when it is paid
//!
//! A selection walks the catalog once and tests each candidate against the patterns the
//! request stated, so its cost is the product of the two. BOTH factors are bounded, and the
//! caller's one is bounded by the model: `AllowedInstanceTypes` and `ExcludedInstanceTypes`
//! are lists of at most 400 members, each of at most 30 characters and of the
//! `^[a-zA-Z0-9\.\*\-]+$` form (`smithy_query(query="shape",shape="AllowedInstanceTypes")`),
//! which the framework refuses a request past before a handler runs; the catalog's own size
//! is the modelled `InstanceType` enum. The selection is made ONCE, when a group is
//! configured with the requirements ([`super::StoredGroup::requirements_instance_type`]
//! holds what it came to), so the requests that converge a group afterwards read a stored
//! type instead of walking the catalog again.

use ::service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::ops::types::{InstanceRequirements, LaunchTemplateOverrides, MixedInstancesPolicy};

/// `DesiredCapacityType` = the number of INSTANCES, the default: "by default, Amazon EC2
/// Auto Scaling specifies `units`, which translates into number of instances".
pub const CAPACITY_TYPE_UNITS: &str = "units";

/// `DesiredCapacityType` = vCPUs, so one instance provides as many units as it has vCPUs.
pub const CAPACITY_TYPE_VCPU: &str = "vcpu";

/// `DesiredCapacityType` = memory in MiB, so one instance provides as many units as it has
/// MiB of memory.
pub const CAPACITY_TYPE_MEMORY_MIB: &str = "memory-mib";

/// The MiB in one GiB, for reading a type's published memory (which Amazon EC2 publishes in
/// MiB) against a requirement stated per vCPU in GiB.
const MIB_PER_GIB: f64 = 1024.0;

/// An attribute a requirement may `include`, `exclude` or `require`
/// (`InstanceRequirements$BareMetal` and `$BurstablePerformance`), whose own documented
/// default is "excluded".
const ATTRIBUTE_INCLUDED: &str = "included";

/// The same member's `required`.
const ATTRIBUTE_REQUIRED: &str = "required";

/// `InstanceRequirements$InstanceGenerations`' `current`.
const GENERATION_CURRENT: &str = "current";

/// The same member's `previous`.
const GENERATION_PREVIOUS: &str = "previous";

/// The `CpuManufacturers` value of the processors Amazon Web Services builds itself, which
/// are the `arm64` ones in Amazon EC2's own catalog.
const CPU_MANUFACTURER_AWS: &str = "amazon-web-services";

/// The architecture prefix those processors carry, as Amazon EC2 publishes it.
const ARM_ARCHITECTURE: &str = "arm64";

/// How many units one instance of `instance_type` provides to a group whose capacity is
/// counted in `desired_capacity_type`.
///
/// `None` where the group's unit is the INSTANCE — `units`, or no stated type at all — so
/// the caller falls back to the weight the group states for the type
/// (`LaunchTemplateOverrides$WeightedCapacity`); and `None` for a type Amazon EC2's catalog
/// does not publish, since there is then no attribute of it to count.
#[must_use]
pub fn capacity_units(
    desired_capacity_type: Option<&str>,
    instance_type: Option<&str>,
) -> Option<i32> {
    let desired_capacity_type = desired_capacity_type?;
    if desired_capacity_type != CAPACITY_TYPE_VCPU
        && desired_capacity_type != CAPACITY_TYPE_MEMORY_MIB
    {
        return None;
    }
    let record = ec2::instance_type_record(instance_type?)?;
    let units = if desired_capacity_type == CAPACITY_TYPE_VCPU {
        record.default_vcpus
    } else {
        i32::try_from(record.memory_mib).unwrap_or(i32::MAX)
    };
    Some(units.max(1))
}

/// The instance type ONE override of a mixed instances policy launches: the type it names,
/// or the type attribute-based selection makes of the requirements it states instead.
///
/// THE ONE reading of an override's launch type in this crate, so the type a launch
/// produces ([`super::ec2_instances`]) and the type the group's capacity arithmetic counts
/// it as ([`super::StoredGroup::launch_capacity_units`]) are the same type.
#[must_use]
pub fn override_instance_type(entry: &LaunchTemplateOverrides) -> Option<String> {
    entry.instance_type.clone().or_else(|| {
        entry
            .instance_requirements
            .as_ref()
            .and_then(selected_instance_type)
    })
}

/// The override a launch from `policy` is made from: the FIRST of its overrides, the way an
/// allocation strategy prefers the first type it is given.
#[must_use]
pub fn launch_override(policy: Option<&MixedInstancesPolicy>) -> Option<&LaunchTemplateOverrides> {
    policy?.launch_template.as_ref()?.overrides.as_ref()?.first()
}

/// Whether a launch from `policy` chooses its instance type by ATTRIBUTES rather than by
/// name: "if you specify `InstanceRequirements`, you can't specify `InstanceType`"
/// (`LaunchTemplateOverrides$InstanceRequirements`), so an override stating requirements and
/// no type is a launch source whose type the requirements decide.
#[must_use]
pub fn selects_by_requirements(policy: Option<&MixedInstancesPolicy>) -> bool {
    launch_override(policy).is_some_and(|entry| {
        entry.instance_type.is_none() && entry.instance_requirements.is_some()
    })
}

/// Whether ANY override of `policy` states `InstanceRequirements`, so the group is one
/// "that specif[ies] InstanceRequirements" — which is the kind of group the allocation
/// strategies document their own defaults per
/// (`smithy_query(query="doc",shape="InstancesDistribution",member="OnDemandAllocationStrategy")`).
///
/// Asked of every override rather than of the one a launch is made from
/// ([`launch_override`]), because the question is about the POLICY the group runs under.
#[must_use]
pub fn states_requirements(policy: &MixedInstancesPolicy) -> bool {
    policy
        .launch_template
        .as_ref()
        .and_then(|template| template.overrides.as_ref())
        .is_some_and(|overrides| {
            overrides
                .iter()
                .any(|entry| entry.instance_requirements.is_some())
        })
}

/// The type `policy`'s attribute-based selection comes to, resolved ONCE when a group is
/// configured with that policy ([`super::StoredGroup::requirements_instance_type`]).
///
/// `None` for a policy that names its type outright or states no requirements at all, and
/// `None` for requirements no offered type satisfies — the two are told apart by
/// [`selects_by_requirements`], which is what makes the second a launch that fails rather
/// than one made from some other type.
#[must_use]
pub fn policy_requirements_type(policy: Option<&MixedInstancesPolicy>) -> Option<String> {
    let entry = launch_override(policy)?;
    if entry.instance_type.is_some() {
        return None;
    }
    entry
        .instance_requirements
        .as_ref()
        .and_then(selected_instance_type)
}

/// The instance type attribute-based selection launches for `requirements`.
///
/// The pool is every type Amazon EC2's own catalog publishes that satisfies the
/// requirements ([`satisfies`]), and the one chosen out of it is the CHEAPEST at the
/// account's own On-Demand rate ([`ec2::on_demand_hour_micros`]), which is what the default
/// allocation strategy asks for: "`lowest-price` … Auto Scaling launches instances using
/// the On-Demand pools with the lowest price"
/// (`smithy_query(query="doc",shape="InstancesDistribution",member="OnDemandAllocationStrategy")`).
/// Types priced the same are taken in the catalog's own order, so one set of requirements
/// always selects one type.
///
/// `None` when no offered type satisfies the requirements: there is then nothing for such a
/// launch source to launch, and the launch records why instead of placing an instance of a
/// type the requirements exclude ([`super::ec2_instances::launch_failure_reason`]).
#[must_use]
pub fn selected_instance_type(requirements: &InstanceRequirements) -> Option<String> {
    ec2::offered_instance_types()
        .iter()
        .filter_map(|instance_type| ec2::instance_type_record(instance_type))
        .filter(|record| satisfies(record, requirements))
        .min_by_key(|record| ec2::on_demand_hour_micros(record, false))
        .map(|record| record.instance_type)
}

/// Whether the instance type `record` publishes satisfies every requirement `requirements`
/// states about an attribute Amazon EC2 publishes for it.
fn satisfies(record: &ec2::InstanceTypeRecord, requirements: &InstanceRequirements) -> bool {
    // "The instance types to apply your specified attributes against … you can use strings
    // with one or more wild cards, represented by an asterisk (*)"
    // (`InstanceRequirements$AllowedInstanceTypes`), and the same wild cards exclude a type
    // on `$ExcludedInstanceTypes`.
    if let Some(allowed) = requirements.allowed_instance_types.as_deref() {
        if !allowed
            .iter()
            .any(|pattern| matches_pattern(pattern, &record.instance_type))
        {
            return false;
        }
    }
    if let Some(excluded) = requirements.excluded_instance_types.as_deref() {
        if excluded
            .iter()
            .any(|pattern| matches_pattern(pattern, &record.instance_type))
        {
            return false;
        }
    }
    // "The minimum and maximum number of vCPUs for an instance type"
    // (`InstanceRequirements$VCpuCount`), whose minimum is required and whose maximum, left
    // out, is unbounded; `$MemoryMiB` states the memory the same way.
    if record.default_vcpus < requirements.v_cpu_count.min {
        return false;
    }
    if requirements
        .v_cpu_count
        .max
        .is_some_and(|max| record.default_vcpus > max)
    {
        return false;
    }
    if record.memory_mib < i64::from(requirements.memory_mi_b.min) {
        return false;
    }
    if requirements
        .memory_mi_b
        .max
        .is_some_and(|max| record.memory_mib > i64::from(max))
    {
        return false;
    }
    if let Some(ratio) = requirements.memory_gi_b_per_v_cpu.as_ref() {
        let per_vcpu = memory_gib_per_vcpu(record);
        if ratio.min.is_some_and(|min| per_vcpu < min) || ratio.max.is_some_and(|max| per_vcpu > max)
        {
            return false;
        }
    }
    // "Indicates whether bare metal instance types are included, excluded, or required …
    // default: excluded" (`InstanceRequirements$BareMetal`), and `$BurstablePerformance`
    // states the burstable T types the same way.
    if !attribute_admits(requirements.bare_metal.as_deref(), record.bare_metal) {
        return false;
    }
    if !attribute_admits(
        requirements.burstable_performance.as_deref(),
        record.burstable,
    ) {
        return false;
    }
    // "Indicates whether current or previous generation instance types are included"
    // (`InstanceRequirements$InstanceGenerations`).
    if let Some(generations) = requirements.instance_generations.as_deref() {
        let generation = if record.current_generation {
            GENERATION_CURRENT
        } else {
            GENERATION_PREVIOUS
        };
        if !generations.iter().any(|wanted| wanted == generation) {
            return false;
        }
    }
    // "Lists which specific CPU manufacturers to include" (`$CpuManufacturers`). What the
    // catalog publishes of a type's processor is its ARCHITECTURE, and the processors Amazon
    // Web Services builds itself are its `arm64` ones, so that is the one manufacturer a
    // requirement can be taken against here.
    if let Some(manufacturers) = requirements.cpu_manufacturers.as_deref() {
        let aws_processor = record.architecture.starts_with(ARM_ARCHITECTURE);
        let aws_wanted = manufacturers
            .iter()
            .any(|manufacturer| manufacturer == CPU_MANUFACTURER_AWS);
        let others_wanted = manufacturers
            .iter()
            .any(|manufacturer| manufacturer != CPU_MANUFACTURER_AWS);
        if aws_processor && !aws_wanted {
            return false;
        }
        if !aws_processor && !others_wanted {
            return false;
        }
    }
    // "Indicates whether instance types must provide On-Demand Instance hibernation
    // support" (`$RequireHibernateSupport`).
    if requirements.require_hibernate_support == Some(true) && !record.hibernation_supported {
        return false;
    }
    true
}

/// The GiB of memory one vCPU of `record`'s type carries, which is what
/// `InstanceRequirements$MemoryGiBPerVCpu` bounds.
fn memory_gib_per_vcpu(record: &ec2::InstanceTypeRecord) -> f64 {
    let memory_mib = f64::from(i32::try_from(record.memory_mib).unwrap_or(i32::MAX));
    memory_mib / MIB_PER_GIB / f64::from(record.default_vcpus.max(1))
}

/// Whether a requirement stated as `included`, `excluded` or `required` admits a type that
/// `has` the attribute: left out it is "excluded", which is the member's own documented
/// default.
fn attribute_admits(requirement: Option<&str>, has: bool) -> bool {
    match requirement {
        Some(ATTRIBUTE_INCLUDED) => true,
        Some(ATTRIBUTE_REQUIRED) => has,
        // `excluded`, and the default every one of these members documents.
        _ => !has,
    }
}

/// Whether `instance_type` matches one entry of an allowed or excluded list, where `*`
/// stands for any run of characters and `?` for any one character — the wild cards those
/// members document.
///
/// Taken through the tree's ONE wildcard walk
/// ([`::service_common::iam_authorization::wildcard_match`]), which is the same single-pass,
/// non-recursive match with one backtrack point per `*`; an instance type is compared as
/// written, so the comparison is case-sensitive.
fn matches_pattern(pattern: &str, instance_type: &str) -> bool {
    ::service_common::iam_authorization::wildcard_match(pattern, instance_type, true)
}
