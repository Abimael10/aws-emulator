// SPDX-License-Identifier: CC0-1.0

//! The published rows of the two Intel storage optimized families, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! Two families whose local NVMe devices ARE the family, a hardware generation apart: the
//! Broadwell-era one on the Xen hypervisor, whose devices are the share of the host's eight
//! 1,900 GB devices each size holds, and the seventh generation one on the Nitro System, whose
//! devices are the share of the host's twelve 3,750 GB devices — one device of a size of its own
//! up to the `4xlarge` and whole devices above it.
//!
//! What they publish that no function of the size yields:
//!
//! * their EBS-optimized mode and the figures it carries. The pre-Nitro family has ONE figure
//!   per size rather than a baseline it bursts above, which is why each of its rows reports its
//!   maximum AS its baseline, and that mode is on whether or not the launch asks for it; the
//!   Nitro family publishes a baseline it bursts above up to 10,000 Mbps and 40,000 IOPS below
//!   the `8xlarge`, and dedicated bandwidth from that size up, reaching the host's own 60,000
//!   Mbps and 240,000 IOPS on the two sizes that hold it.
//! * how many EBS volumes a size holds attached, and whether that limit is the volumes' own.
//!   Every VIRTUALIZED size of the pre-Nitro family shares one pool of 40 attachments with its
//!   interfaces and its local devices, while the published figure for the whole host its bare
//!   metal size holds is not recorded here, so that row publishes none and the derivation
//!   answers it; the Nitro family's volumes are attached to EBS hardware of the
//!   type's own, so each virtualized size reports the `dedicated` kind — 32 volumes up to the
//!   `12xlarge`, then 48 and 64 as the size grows and 128 on the size that holds the whole host
//!   — while a bare metal size shares its host's one pool between every device and reports that
//!   pool: 79 attachments on the whole host and 39 on the half of it the smaller one holds.
//! * their network performance as the published column writes it, their processors' sustained
//!   frequencies, and the host features each is offered. On-Demand hibernation IS offered on
//!   both families up to the memory the hibernation prerequisites bound an instance at, so the
//!   sizes above it are not offered it; ENA Express is offered on the Nitro family from the
//!   `12xlarge` up and on both of its bare metal sizes, the sizes under it carrying less
//!   dedicated bandwidth than its transport needs; the number of ENA
//!   queues cannot be changed on its adapter and it takes no interface attachment from a
//!   secondary network, so no private address at all is available on one; its processor runs a
//!   hypervisor inside the guest, so every virtualized size publishes nested virtualization as a
//!   processor feature, which a bare metal size — running the caller's software with no
//!   hypervisor under it — does not; the Elastic Fabric Adapter is offered on its two widest
//!   virtualized sizes and on the bare metal size that holds the whole host; no size of it is
//!   offered reboot migration, the local data of an instance store being what such a migration
//!   could not carry over, while a local PTP hardware clock is offered on every size; and each
//!   of its bare metal sizes is offered the legacy BIOS boot mode alone and publishes no
//!   trusted platform module.
//! * the core counts a size publishes: the pre-Nitro family's widest size carries the cores of
//!   TWO processor sockets and takes them one per socket, so its counts go up in twos, and the
//!   Nitro family's widest virtualized size takes its cores all at once, so the one count it
//!   publishes is its default.
//!
//! The figures come from each family's own instance-type specification page (vCPUs, memory, the
//! local devices, the network performance), the "Amazon EBS optimized instance types" table
//! (the four EBS figures per size), "Instance volume limits" (how many volumes a size holds
//! attached and whether that limit is the volumes' own) and the "CPU cores and threads per CPU
//! core per instance type" table (the core counts a size publishes).

use super::*;

/// The sustained all-core frequency, in GHz, of the Broadwell part the pre-Nitro storage family
/// runs on.
const BROADWELL_STORAGE_SUSTAINED_GHZ: f64 = 2.3;

/// The sustained all-core frequency, in GHz, of the seventh generation Intel part the Nitro
/// storage family runs on.
const SEVENTH_GENERATION_SUSTAINED_GHZ: f64 = 3.2;

/// The EBS volumes a size of the pre-Nitro family holds attached: the limit its hypervisor
/// allows, shared with its interfaces and its local devices.
const XEN_STORAGE_EBS_ATTACHMENTS: i32 = 40;

/// The `EbsOptimizedSupport` of a family whose EBS-optimized mode is always on.
const EBS_OPTIMIZED_BY_DEFAULT: &str = "default";

/// The size, in GB, of ONE local NVMe device of the pre-Nitro family: a store bigger than this
/// is more devices of this size, and a size whose share of the host's storage is below it holds
/// one device of that share.
const BROADWELL_LOCAL_DEVICE_GB: i64 = 1_900;

/// The same figure for the seventh generation family, whose devices are twice as large.
const SEVENTH_GENERATION_LOCAL_DEVICE_GB: i64 = 3_750;

/// "The network performance" a size that holds the WHOLE seventh generation host publishes:
/// its adapter's own ceiling, which it bursts up to.
const WHOLE_HOST_NETWORK_PERFORMANCE: &str = "Up to 100 Gigabit";

/// The published hardware facts of the two Intel storage optimized families' instance types,
/// one row per type.
pub(super) static INTEL_STORAGE_OPTIMIZED_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // I3 — storage optimized on an Intel Xeon E5-2686 v4 (Broadwell) at a sustained 2.3 GHz,
    // carrying the local NVMe devices the family is built around. Its EBS-optimized mode is on
    // at every size and carries ONE figure rather than a baseline it bursts above, those figures
    // being the family's rows of "Amazon EBS optimized instance types": 425 Mbps and 3,000 IOPS
    // per `large` of size up to the `2xlarge`, and 3,500/16,000, 7,000/32,500 and 14,000/65,000
    // above it, which no uniform slope produces. The volume limit of 40 its hypervisor allows is
    // shared with its interfaces and its devices. On-Demand hibernation is offered up to the
    // memory the hibernation prerequisites bound an instance at, so the two widest sizes — which
    // carry more — are not offered it, and the widest virtualized size carries the cores of two
    // processor sockets, taken one per socket, so its published core counts go up in twos. The
    // bare metal size is the whole host: 72 vCPUs and 512 GiB, more than the ladder's step above
    // the widest virtualized size, with the host's own EBS figures.
    PublishedInstanceTypeFacts::of("i3.large")
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(425, 3_000, 425, 3_000)
        .shared_ebs_attachments(XEN_STORAGE_EBS_ATTACHMENTS)
        .network("Up to 10 Gigabit")
        .processor("Intel", BROADWELL_STORAGE_SUSTAINED_GHZ)
        .hibernation(true),
    PublishedInstanceTypeFacts::of("i3.xlarge")
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(850, 6_000, 850, 6_000)
        .shared_ebs_attachments(XEN_STORAGE_EBS_ATTACHMENTS)
        .network("Up to 10 Gigabit")
        .processor("Intel", BROADWELL_STORAGE_SUSTAINED_GHZ)
        .hibernation(true),
    PublishedInstanceTypeFacts::of("i3.2xlarge")
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(1_700, 12_000, 1_700, 12_000)
        .shared_ebs_attachments(XEN_STORAGE_EBS_ATTACHMENTS)
        .network("Up to 10 Gigabit")
        .processor("Intel", BROADWELL_STORAGE_SUSTAINED_GHZ)
        .hibernation(true),
    PublishedInstanceTypeFacts::of("i3.4xlarge")
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(3_500, 16_000, 3_500, 16_000)
        .shared_ebs_attachments(XEN_STORAGE_EBS_ATTACHMENTS)
        .network("Up to 10 Gigabit")
        .processor("Intel", BROADWELL_STORAGE_SUSTAINED_GHZ)
        .hibernation(true),
    PublishedInstanceTypeFacts::of("i3.8xlarge")
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(7_000, 32_500, 7_000, 32_500)
        .shared_ebs_attachments(XEN_STORAGE_EBS_ATTACHMENTS)
        .network("10 Gigabit")
        // The ONE size of this family whose published figure is not the share of its host the
        // size ladder gives it (12.5), so the row publishes the figure in Gbps as well: the
        // network cards carry what the column says rather than a share that contradicts it.
        // Every other size's published column IS that share, so their rows publish the text
        // alone and the cards read the same figure off the ladder.
        .network_bandwidth(10.0, 10.0)
        .processor("Intel", BROADWELL_STORAGE_SUSTAINED_GHZ),
    PublishedInstanceTypeFacts::of("i3.16xlarge")
        .cores_in_steps_of(2)
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(14_000, 65_000, 14_000, 65_000)
        .shared_ebs_attachments(XEN_STORAGE_EBS_ATTACHMENTS)
        .network("25 Gigabit")
        .processor("Intel", BROADWELL_STORAGE_SUSTAINED_GHZ),
    PublishedInstanceTypeFacts::of("i3.metal")
        .vcpus(72)
        .memory_gib(512)
        .cores_in_steps_of(2)
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(19_000, 80_000, 19_000, 80_000)
        .instance_store(8, BROADWELL_LOCAL_DEVICE_GB)
        .network("25 Gigabit")
        .processor("Intel", BROADWELL_STORAGE_SUSTAINED_GHZ),
    // I7i — storage optimized on the seventh generation Intel part at a sustained 3.2 GHz,
    // carrying the 3,750 GB local NVMe devices of a 192-vCPU host: one device of the share of
    // that store the size holds up to the `4xlarge`, and whole devices of that size above it, so
    // the `24xlarge` holds six and the size that holds the whole host twelve. Its EBS figures
    // are the family's rows of "Amazon EBS optimized instance types" — 312.5 Mbps and 1,250 IOPS
    // per vCPU, bursting up to 10,000 Mbps and 40,000 IOPS below the `8xlarge` and dedicated
    // from that size up, reaching 60,000 Mbps and 240,000 IOPS on the sizes that hold the whole
    // host. Its network figures are the published ones, and EVERY size of the family bursts
    // rather than carrying a dedicated share: up to 10 Gigabit at the two smallest sizes, 12 at
    // the `2xlarge` and the 25 its adapter reaches at the `4xlarge` and the `8xlarge`, then the
    // size's own share of the host at 0.586 Gigabit per vCPU — 28.12, 37.5 and 56.25 — and the
    // adapter's own 100 Gigabit on the sizes that hold the whole host, which is below the 112.5
    // that share would give them. ENA Express is offered from the `12xlarge` up and on both bare
    // metal sizes, the sizes under it carrying less network bandwidth than its transport needs.
    // No size is offered reboot migration — the local data of an instance store cannot be
    // carried over to another host — while a local PTP hardware clock is offered on every size,
    // the bare metal ones included; and each bare metal size is offered the legacy BIOS boot mode
    // alone and publishes NO trusted platform module, the published list of the instance types
    // offered NitroTPM leaving every bare metal instance out of it.
    PublishedInstanceTypeFacts::of("i7i.large")
        .ebs(625, 2_500, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .instance_store(1, 468)
        .network("Up to 10 Gigabit")
        .processor("Intel", SEVENTH_GENERATION_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(false)
        .hibernation(true)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .flexible_ena_queues(false),
    PublishedInstanceTypeFacts::of("i7i.xlarge")
        .ebs(1_250, 5_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .instance_store(1, 937)
        .network("Up to 10 Gigabit")
        .processor("Intel", SEVENTH_GENERATION_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(false)
        .hibernation(true)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .flexible_ena_queues(false),
    PublishedInstanceTypeFacts::of("i7i.2xlarge")
        .ebs(2_500, 10_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .instance_store(1, 1_875)
        .network("Up to 12 Gigabit")
        .processor("Intel", SEVENTH_GENERATION_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(false)
        .hibernation(true)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .flexible_ena_queues(false),
    PublishedInstanceTypeFacts::of("i7i.4xlarge")
        .ebs(5_000, 20_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .instance_store(1, SEVENTH_GENERATION_LOCAL_DEVICE_GB)
        .network("Up to 25 Gigabit")
        .processor("Intel", SEVENTH_GENERATION_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(false)
        .hibernation(true)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .flexible_ena_queues(false),
    PublishedInstanceTypeFacts::of("i7i.8xlarge")
        .ebs(10_000, 40_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .instance_store(2, SEVENTH_GENERATION_LOCAL_DEVICE_GB)
        .network("Up to 25 Gigabit")
        .processor("Intel", SEVENTH_GENERATION_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(false)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .flexible_ena_queues(false),
    PublishedInstanceTypeFacts::of("i7i.12xlarge")
        .ebs(15_000, 60_000, 15_000, 60_000)
        .dedicated_ebs_attachments(32)
        .instance_store(3, SEVENTH_GENERATION_LOCAL_DEVICE_GB)
        // The share this size holds of the host's network, as the sizes either side of it
        // publish theirs: 0.586 Gigabit per vCPU, in the hundredths the published column writes.
        .network("Up to 28.12 Gigabit")
        .processor("Intel", SEVENTH_GENERATION_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(true)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .flexible_ena_queues(false),
    PublishedInstanceTypeFacts::of("i7i.16xlarge")
        .ebs(20_000, 80_000, 20_000, 80_000)
        .dedicated_ebs_attachments(48)
        .instance_store(4, SEVENTH_GENERATION_LOCAL_DEVICE_GB)
        .network("Up to 37.5 Gigabit")
        .processor("Intel", SEVENTH_GENERATION_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(true)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .flexible_ena_queues(false),
    PublishedInstanceTypeFacts::of("i7i.24xlarge")
        .ebs(30_000, 120_000, 30_000, 120_000)
        .dedicated_ebs_attachments(64)
        .instance_store(6, SEVENTH_GENERATION_LOCAL_DEVICE_GB)
        .network("Up to 56.25 Gigabit")
        .efa(1)
        .processor("Intel", SEVENTH_GENERATION_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(true)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .flexible_ena_queues(false),
    PublishedInstanceTypeFacts::of("i7i.48xlarge")
        .cores_all_at_once()
        .ebs(60_000, 240_000, 60_000, 240_000)
        .dedicated_ebs_attachments(128)
        .instance_store(12, SEVENTH_GENERATION_LOCAL_DEVICE_GB)
        .network(WHOLE_HOST_NETWORK_PERFORMANCE)
        .efa(1)
        .processor("Intel", SEVENTH_GENERATION_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(true)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .flexible_ena_queues(false),
    PublishedInstanceTypeFacts::of("i7i.metal-24xl")
        .ebs(30_000, 120_000, 30_000, 120_000)
        .shared_ebs_attachments(39)
        .instance_store(6, SEVENTH_GENERATION_LOCAL_DEVICE_GB)
        .network("Up to 56.25 Gigabit")
        .processor("Intel", SEVENTH_GENERATION_SUSTAINED_GHZ)
        .ena_srd(true)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .flexible_ena_queues(false),
    PublishedInstanceTypeFacts::of("i7i.metal-48xl")
        .ebs(60_000, 240_000, 60_000, 240_000)
        .shared_ebs_attachments(79)
        .instance_store(12, SEVENTH_GENERATION_LOCAL_DEVICE_GB)
        .network(WHOLE_HOST_NETWORK_PERFORMANCE)
        .efa(1)
        .processor("Intel", SEVENTH_GENERATION_SUSTAINED_GHZ)
        .ena_srd(true)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .flexible_ena_queues(false),
];
