// SPDX-License-Identifier: CC0-1.0

//! The published rows of the accelerated families whose accelerators are GPUs, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! What a GPU family publishes that no size multiplier yields: the accelerators themselves
//! (`InstanceTypeInfo$GpuInfo` — how many devices, which device and how much memory each
//! carries), the local NVMe devices the family ships with, and the host features an
//! accelerated type is NOT offered — CloudWatch action based recovery cannot carry the data of
//! a local instance store over, and the family is not offered on a Dedicated Host.
//!
//! The figures come from the family's own instance-type specification page (accelerators,
//! vCPUs, memory, local devices, network performance), the "Amazon EBS optimized instance
//! types" table (the four EBS figures) and "Instance volume limits" (how many volumes a type
//! holds attached and whether that limit is the volumes' own).

use super::*;

/// The `GpuDeviceInfo$Name` of the accelerator the sixth generation graphics families carry,
/// and its published memory in MiB: a 24 GB device.
const L4_ACCELERATOR: (&str, i32) = ("L4", 22_888);

/// The accelerator the fifth generation x86 graphics family carries, and its published memory
/// in MiB: the same 24 GB of the device above, this one the generation before it.
const A10G_ACCELERATOR: (&str, i32) = ("A10G", 22_888);

/// The accelerator the AWS Graviton graphics family carries, and its published memory in MiB:
/// a 16 GB device.
const T4G_ACCELERATOR: (&str, i32) = ("T4g", 16_384);

/// The `GpuDeviceInfo$Manufacturer` of every accelerator here.
const NVIDIA: &str = "NVIDIA";

/// The published hardware facts of the GPU families' instance types, one row per type.
pub(super) static GPU_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // Gr6 — the graphics family's memory-rich pair, 3rd generation AMD EPYC at a sustained
    // 3.6 GHz with ONE L4 accelerator whatever the size, 8 GiB of system memory per vCPU and
    // local NVMe devices whose count and size are the type's own: one 600 GB device on the
    // `4xlarge` and two 450 GB ones on the `8xlarge`. Its EBS volumes are attached to EBS
    // hardware of the type's own rather than out of the pool shared with its interfaces, so
    // the 32 attachments each size reports are the volumes' own; the `4xlarge` publishes
    // 8,000 Mbps and 32,000 IOPS and the `8xlarge` twice that. Neither size is offered on a
    // Dedicated Host, ENA Express is not offered on the family, and the Elastic Fabric Adapter
    // is offered on the larger size alone, which also has the whole 25 Gigabit of its share of
    // the host's network rather than bursting up to it.
    PublishedInstanceTypeFacts::of("gr6.4xlarge")
        .ebs(8_000, 32_000, 8_000, 32_000)
        .dedicated_ebs_attachments(32)
        .instance_store(1, 600)
        .network("Up to 25 Gigabit")
        .gpus(1, L4_ACCELERATOR.0, NVIDIA, L4_ACCELERATOR.1, ML_AI_WORKLOAD)
        .processor("AMD", 3.6)
        .dedicated_hosts(false)
        .ena_srd(false),
    PublishedInstanceTypeFacts::of("gr6.8xlarge")
        .ebs(16_000, 64_000, 16_000, 64_000)
        .dedicated_ebs_attachments(32)
        .instance_store(2, 450)
        .network("25 Gigabit")
        .efa(1)
        .gpus(1, L4_ACCELERATOR.0, NVIDIA, L4_ACCELERATOR.1, ML_AI_WORKLOAD)
        .processor("AMD", 3.6)
        .dedicated_hosts(false)
        .ena_srd(false),
    // G5 — the fifth generation graphics family, NVIDIA A10G accelerators on a 2nd generation
    // AMD EPYC (7R32) at a sustained 3.3 GHz, 4 GiB of system memory per vCPU and ONE local
    // NVMe device at every size but the largest, whose size is the type's own rather than a
    // multiple of the family's `large`. It is NOT the latest generation of its family, and the
    // Dedicated Host it is offered on carries the whole host's accelerators, so only the two
    // sizes that hold four or more of them are offered one. Its adapter encrypts in-transit
    // traffic between instances, and the Elastic Fabric Adapter is offered from the size that
    // holds a dedicated share of the host's network up. Its EBS volumes come out of the shared
    // Nitro attachment pool, which the accelerators themselves also draw on, so the published
    // limit is below what the interfaces and the local devices alone would leave. The two
    // sizes with four accelerators and the one with eight take their interfaces from the
    // host's own share rather than their step of the size ladder. No size of the family is
    // migrated to another host on a user-initiated reboot, and none carries a local clock.
    // The family is NOT the latest generation of its family: the recorded catalog reports
    // every size of it with that flag false, the G ladder having moved past it, which is why
    // each row below says so and why the family is named among the previous generation ones
    // that are still offered IPv6 addresses per interface.
    PublishedInstanceTypeFacts::of("g5.xlarge")
        .previous_generation()
        .ebs(700, 3_000, 3_500, 15_000)
        .ebs_attachments(25)
        .instance_store(1, 250)
        .network("Up to 10 Gigabit")
        .gpus(
            1,
            A10G_ACCELERATOR.0,
            NVIDIA,
            A10G_ACCELERATOR.1,
            ML_AI_WORKLOAD,
        )
        .processor("AMD", 3.3)
        .encryption_in_transit(true)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("g5.2xlarge")
        .previous_generation()
        .ebs(850, 3_500, 3_500, 15_000)
        .ebs_attachments(25)
        .instance_store(1, 450)
        .network("Up to 10 Gigabit")
        .gpus(
            1,
            A10G_ACCELERATOR.0,
            NVIDIA,
            A10G_ACCELERATOR.1,
            ML_AI_WORKLOAD,
        )
        .processor("AMD", 3.3)
        .encryption_in_transit(true)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("g5.4xlarge")
        .previous_generation()
        .ebs(4_750, 20_000, 4_750, 20_000)
        .ebs_attachments(25)
        .instance_store(1, 600)
        .network("Up to 25 Gigabit")
        .gpus(
            1,
            A10G_ACCELERATOR.0,
            NVIDIA,
            A10G_ACCELERATOR.1,
            ML_AI_WORKLOAD,
        )
        .processor("AMD", 3.3)
        .encryption_in_transit(true)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("g5.8xlarge")
        .previous_generation()
        .ebs(16_000, 65_000, 16_000, 65_000)
        .ebs_attachments(25)
        .instance_store(1, 900)
        .network_bandwidth(25.0, 25.0)
        .efa(1)
        .gpus(
            1,
            A10G_ACCELERATOR.0,
            NVIDIA,
            A10G_ACCELERATOR.1,
            ML_AI_WORKLOAD,
        )
        .processor("AMD", 3.3)
        .encryption_in_transit(true)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("g5.12xlarge")
        .previous_generation()
        .ebs(16_000, 65_000, 16_000, 65_000)
        .ebs_attachments(22)
        .instance_store(1, 3_800)
        .network_bandwidth(40.0, 40.0)
        .interfaces(15)
        .ipv4_per_interface(50)
        .efa(1)
        .gpus(
            4,
            A10G_ACCELERATOR.0,
            NVIDIA,
            A10G_ACCELERATOR.1,
            ML_AI_WORKLOAD,
        )
        .processor("AMD", 3.3)
        .encryption_in_transit(true)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("g5.16xlarge")
        .previous_generation()
        .ebs(16_000, 65_000, 16_000, 65_000)
        .ebs_attachments(25)
        .instance_store(1, 1_900)
        .network_bandwidth(25.0, 25.0)
        .interfaces(8)
        .ipv4_per_interface(30)
        .efa(1)
        .gpus(
            1,
            A10G_ACCELERATOR.0,
            NVIDIA,
            A10G_ACCELERATOR.1,
            ML_AI_WORKLOAD,
        )
        .processor("AMD", 3.3)
        .encryption_in_transit(true)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("g5.24xlarge")
        .previous_generation()
        .ebs(19_000, 80_000, 19_000, 80_000)
        .ebs_attachments(22)
        .instance_store(1, 3_800)
        .network_bandwidth(50.0, 50.0)
        .efa(1)
        .gpus(
            4,
            A10G_ACCELERATOR.0,
            NVIDIA,
            A10G_ACCELERATOR.1,
            ML_AI_WORKLOAD,
        )
        .processor("AMD", 3.3)
        .dedicated_hosts(true)
        .encryption_in_transit(true)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("g5.48xlarge")
        .previous_generation()
        .ebs(19_000, 80_000, 19_000, 80_000)
        // Eight accelerators and two local devices, each drawing on the same pool the volumes
        // come out of, which is what leaves this size the fewest volumes of the family. This
        // is the family's own rule applied to this size — the rule the recorded figures of
        // every other size of this family and of the Graviton one below confirm — and NOT the
        // recorded figure for this one size, which that rule does not yield: AWS publishes a
        // limit of its own for the size that holds the whole accelerated host, and no figure
        // derivable from the other rows produces it.
        .ebs_attachments(17)
        .instance_store(2, 3_800)
        .network_bandwidth(100.0, 100.0)
        .interfaces(7)
        .efa(1)
        .gpus(
            8,
            A10G_ACCELERATOR.0,
            NVIDIA,
            A10G_ACCELERATOR.1,
            ML_AI_WORKLOAD,
        )
        .processor("AMD", 3.3)
        .dedicated_hosts(true)
        .encryption_in_transit(true)
        .host_features(false, false),
    // G5g — the graphics family on AWS Graviton2 at a sustained 2.5 GHz with NVIDIA T4g
    // accelerators: 2 GiB of system memory per vCPU, HALF the general purpose ratio its family
    // letter would give, and NO local instance store at any size, which is also why CloudWatch
    // action based recovery is offered on every virtualized size. Its two largest sizes carry
    // two accelerators where the rest carry one. The Dedicated Host carries the whole host, so
    // only the sizes that hold it are offered one. Its EBS volumes come out of the shared
    // Nitro attachment pool, which the accelerators draw on too.
    PublishedInstanceTypeFacts::of("g5g.xlarge")
        .memory_gib(8)
        .no_instance_store()
        .ebs(1_188, 6_000, 4_750, 20_000)
        .ebs_attachments(26)
        .network("Up to 10 Gigabit")
        .gpus(
            1,
            T4G_ACCELERATOR.0,
            NVIDIA,
            T4G_ACCELERATOR.1,
            ML_AI_WORKLOAD,
        )
        .processor("AWS", 2.5)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("g5g.2xlarge")
        .memory_gib(16)
        .no_instance_store()
        .ebs(2_375, 12_000, 4_750, 20_000)
        .ebs_attachments(26)
        .network("Up to 10 Gigabit")
        .gpus(
            1,
            T4G_ACCELERATOR.0,
            NVIDIA,
            T4G_ACCELERATOR.1,
            ML_AI_WORKLOAD,
        )
        .processor("AWS", 2.5)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("g5g.4xlarge")
        .memory_gib(32)
        .no_instance_store()
        .ebs(4_750, 20_000, 4_750, 20_000)
        .ebs_attachments(26)
        .network("Up to 10 Gigabit")
        .gpus(
            1,
            T4G_ACCELERATOR.0,
            NVIDIA,
            T4G_ACCELERATOR.1,
            ML_AI_WORKLOAD,
        )
        .processor("AWS", 2.5)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("g5g.8xlarge")
        .memory_gib(64)
        .no_instance_store()
        .ebs(9_500, 40_000, 9_500, 40_000)
        .ebs_attachments(26)
        .network_bandwidth(12.0, 12.0)
        .gpus(
            1,
            T4G_ACCELERATOR.0,
            NVIDIA,
            T4G_ACCELERATOR.1,
            ML_AI_WORKLOAD,
        )
        .processor("AWS", 2.5)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("g5g.16xlarge")
        .memory_gib(128)
        .no_instance_store()
        .ebs(19_000, 80_000, 19_000, 80_000)
        .ebs_attachments(25)
        .network_bandwidth(25.0, 25.0)
        .gpus(
            2,
            T4G_ACCELERATOR.0,
            NVIDIA,
            T4G_ACCELERATOR.1,
            ML_AI_WORKLOAD,
        )
        .processor("AWS", 2.5),
    PublishedInstanceTypeFacts::of("g5g.metal")
        .memory_gib(128)
        .no_instance_store()
        .ebs(19_000, 80_000, 19_000, 80_000)
        // A bare metal instance has no hypervisor passing an accelerator or a local device
        // through to it, so neither draws on the attachment pool its volumes come out of, and
        // the pool itself is the larger one the whole host carries.
        .ebs_attachments(31)
        .network_bandwidth(25.0, 25.0)
        // The packets of a session are copied by the host's own card, which a bare metal
        // instance still has, so this family's whole-host size is offered Traffic Mirroring.
        .traffic_mirroring(true)
        .gpus(
            2,
            T4G_ACCELERATOR.0,
            NVIDIA,
            T4G_ACCELERATOR.1,
            ML_AI_WORKLOAD,
        )
        .processor("AWS", 2.5)
        // The move to another host is the HOST's to make on a bare metal instance, which has
        // no hypervisor to make it, and there is no local clock on this hardware generation.
        .host_features(true, false),
];
