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
];
