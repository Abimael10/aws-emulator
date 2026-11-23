// SPDX-License-Identifier: CC0-1.0

//! The published rows of the seventh generation graphics family, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! What this family publishes that no size multiplier yields: its accelerators
//! (`InstanceTypeInfo$GpuInfo` — a professional NVIDIA device of 32 GiB, one of them up to the
//! `8xlarge` and the whole host's eight on the widest size), the EBS hardware of its own its
//! volumes are attached to (so the limit it reports is the volumes' own rather than a share of
//! the pool its interfaces draw on), the local NVMe device each size carries, the Intel Xeon 6
//! part it runs on — whose cores run ONE thread each, so a size reports its vCPUs as that many
//! cores — and the host features an accelerated type is not offered: it is not offered on a
//! Dedicated Host, an instance of it is not migrated to another host on a user-initiated
//! reboot, it carries no local PTP hardware clock, the packets of a session cannot be copied
//! under its guest, ENA Express is offered on the two sizes that hold half the accelerated host
//! and the whole of it — the narrower sizes carrying less dedicated network bandwidth than its
//! transport needs — and it takes no interface attachment from a secondary network, so no
//! private address at all is available on one. The Elastic Fabric Adapter is offered from the
//! size that holds a dedicated share of the host's network up.
//!
//! The figures come from the family's own instance-type specification page (the accelerators,
//! the local devices and the processor), the "Amazon EBS optimized instance types" table,
//! "Instance volume limits" and "Connection tracking timeouts" (the established-TCP idle
//! timeout of this hardware generation). The recorded `8xlarge`, `12xlarge` and `24xlarge` rows
//! are the ones the ladders here are read from: the EBS figures are the graphics ladder's rows
//! of that table, dedicated from the `4xlarge` up and doubling with the share of the host above
//! the `12xlarge`, the local device is the family's own 1,900 GB NVMe one from the `8xlarge`
//! through the `12xlarge` — 3,800 GB on the size that holds half the host and two of those on
//! the whole of it, while the two narrowest sizes carry a device this table does not publish
//! yet — and the volume limit grows with the share too: 32 volumes up to the `12xlarge`, then
//! 64 and 128, with the widest size spreading its figures over two EBS cards, carrying a
//! network card per processor socket and taking its cores all at once.

use super::*;

/// The `GpuDeviceInfo$Name` of the accelerator this family carries, and its published memory in
/// MiB: a 32 GiB professional device.
const RTX_PRO_4500_ACCELERATOR: (&str, i32) = ("RTX PRO 4500", 32_768);

/// The `GpuDeviceInfo$Manufacturer` of that accelerator.
const NVIDIA: &str = "NVIDIA";

/// The `GpuDeviceInfo$Workloads` of an accelerator offered for BOTH the graphics and the
/// machine learning workload, which is what this family's published inventory names — the
/// device is a professional graphics part that also runs inference.
const GRAPHICS_AND_ML_AI_WORKLOADS: &[&str] = &["graphics", "ml-ai"];

/// The threads one core of this family's processor runs: the part ships with simultaneous
/// multithreading off, so a size's vCPUs ARE its cores.
const ONE_THREAD_PER_CORE: i32 = 1;

/// The published hardware facts of the seventh generation graphics family's instance types,
/// one row per type.
pub(super) static SEVENTH_GENERATION_GRAPHICS_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    PublishedInstanceTypeFacts::of("g7.2xlarge")
        .threads_per_core(ONE_THREAD_PER_CORE)
        // The two sizes below the family's dedicated row sustain their share and burst above
        // it, up to the 8,000 Mbps and 32,000 IOPS of the `4xlarge`, the first row of the
        // ladder whose bandwidth is dedicated.
        .ebs(4_000, 16_000, 8_000, 32_000)
        .dedicated_ebs_attachments(32)
        .gpus(
            1,
            RTX_PRO_4500_ACCELERATOR.0,
            NVIDIA,
            RTX_PRO_4500_ACCELERATOR.1,
            GRAPHICS_AND_ML_AI_WORKLOADS,
        )
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .dedicated_hosts(false)
        .ena_srd(false)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("g7.4xlarge")
        .threads_per_core(ONE_THREAD_PER_CORE)
        .ebs(8_000, 32_000, 8_000, 32_000)
        .dedicated_ebs_attachments(32)
        .gpus(
            1,
            RTX_PRO_4500_ACCELERATOR.0,
            NVIDIA,
            RTX_PRO_4500_ACCELERATOR.1,
            GRAPHICS_AND_ML_AI_WORKLOADS,
        )
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .dedicated_hosts(false)
        .ena_srd(false)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("g7.8xlarge")
        .threads_per_core(ONE_THREAD_PER_CORE)
        .ebs(16_000, 64_000, 16_000, 64_000)
        .dedicated_ebs_attachments(32)
        .instance_store(1, 1_900)
        .efa(1)
        .gpus(
            1,
            RTX_PRO_4500_ACCELERATOR.0,
            NVIDIA,
            RTX_PRO_4500_ACCELERATOR.1,
            GRAPHICS_AND_ML_AI_WORKLOADS,
        )
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .dedicated_hosts(false)
        .ena_srd(false)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("g7.12xlarge")
        .threads_per_core(ONE_THREAD_PER_CORE)
        .ebs(20_000, 80_000, 20_000, 80_000)
        .dedicated_ebs_attachments(32)
        .instance_store(1, 1_900)
        .efa(1)
        // The size that holds a quarter of the accelerated host holds four of its eight
        // accelerators, the published inventory of this family's class being read per share of
        // the host rather than per vCPU.
        .gpus(
            4,
            RTX_PRO_4500_ACCELERATOR.0,
            NVIDIA,
            RTX_PRO_4500_ACCELERATOR.1,
            GRAPHICS_AND_ML_AI_WORKLOADS,
        )
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .dedicated_hosts(false)
        .ena_srd(false)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("g7.24xlarge")
        .threads_per_core(ONE_THREAD_PER_CORE)
        .ebs(40_000, 160_000, 40_000, 160_000)
        .dedicated_ebs_attachments(64)
        .instance_store(1, 3_800)
        .efa(1)
        .gpus(
            4,
            RTX_PRO_4500_ACCELERATOR.0,
            NVIDIA,
            RTX_PRO_4500_ACCELERATOR.1,
            GRAPHICS_AND_ML_AI_WORKLOADS,
        )
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .dedicated_hosts(false)
        // The sizes that hold half the accelerated host and the whole of it carry enough
        // dedicated network bandwidth for ENA Express, which the narrower sizes do not.
        .ena_srd(true)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("g7.48xlarge")
        .threads_per_core(ONE_THREAD_PER_CORE)
        // The size that holds the whole host takes its cores ALL at once, so the one count it
        // can be configured with is its default.
        .cores_all_at_once()
        .ebs(80_000, 320_000, 80_000, 320_000)
        .dedicated_ebs_attachments(128)
        // The size that holds the WHOLE accelerated host carries that host's two EBS cards,
        // which is what spreads its figures over a card each.
        .ebs_cards(2)
        .instance_store(2, 3_800)
        .efa(1)
        .gpus(
            8,
            RTX_PRO_4500_ACCELERATOR.0,
            NVIDIA,
            RTX_PRO_4500_ACCELERATOR.1,
            GRAPHICS_AND_ML_AI_WORKLOADS,
        )
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .dedicated_hosts(false)
        .ena_srd(true)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
];
