// SPDX-License-Identifier: CC0-1.0

//! The published rows of the eighth generation Intel compute families that carry local NVMe
//! devices, read through [`super::published_instance_type_facts`] like every other row of the
//! catalog.
//!
//! This hardware generation's host is TWICE the width of the one before it — 384 vCPUs where
//! the seventh generation's compute host has 192 — so no step of the size ladder lines up
//! with the earlier families' figures: the EBS bandwidth and IOPS, the network performance and
//! the local devices of a size are the host's figures divided by the share that size holds,
//! and the largest sizes reach figures no earlier compute family publishes. Its network and
//! EBS bandwidth is configurable, so every size publishes the weightings a launch may ask for,
//! and its connection tracking applies this generation's own, much shorter established-TCP
//! idle timeout.
//!
//! Each family's comment says where its figures come from: the family's own instance-type
//! specification page (vCPUs, memory, network performance, the local devices), the "Amazon EBS
//! optimized instance types" table (the four EBS figures), "Instance volume limits" (how many
//! volumes a type holds attached and whether that limit is the volumes' own) and the "CPU
//! cores and threads per CPU core per instance type" table (the core counts a size publishes).

use super::*;

/// The published hardware facts of the eighth generation Intel NVMe compute families' instance
/// types, one row per type.
pub(super) static INTEL_NVME_COMPUTE_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // C8id — compute optimized on Intel Xeon 6, at a sustained 3.9 GHz, carrying local NVMe
    // devices. Its EBS figures are the ladder's rows of "Amazon EBS optimized instance types":
    // the sizes below the `8xlarge` publish a baseline they burst above, up to 10,000 Mbps and
    // 40,000 IOPS, and the bandwidth is dedicated from that size up, reaching the host's own
    // 80,000 Mbps and 480,000 IOPS on the two sizes that hold the whole host — the bandwidth
    // stops at the host's ceiling where the IOPS keep doubling with the size. Those volumes
    // are attached to EBS hardware of the type's own rather than out of the pool shared with
    // its interfaces and its devices, so every virtualized size reports the `dedicated` kind of
    // limit — 32 volumes up to the `12xlarge`, then 48 and 64 as the size grows and 128 from
    // the `32xlarge` up — while each bare metal size shares its host's one pool between every
    // device and reports that pool: 79 attachments on the whole host and 39 on the half of it
    // the smaller one holds. Its local store is ONE device of the share of the host's storage
    // the size holds up to the 3,800 GB device this hardware carries, and whole or halved
    // devices of that size above it, so the `96xlarge` and the bare metal size of the same
    // width hold six of them. The network performance is the host's ladder, up to 12.5 Gigabit
    // below the `2xlarge`, up to 15 on the two sizes above them and the size's share of the
    // host's 100 from the `8xlarge` up. ENA Express is offered from the `8xlarge` up, every size
    // from there stating it, the sizes under it carrying less dedicated network bandwidth than
    // its transport needs. The family is
    // not offered On-Demand hibernation at any size, and no size is offered reboot migration:
    // the local data of an instance store cannot be carried over to another host. A local PTP
    // hardware clock is offered on every size, the bare metal ones included. Its processor runs
    // a hypervisor inside the guest, so every virtualized size publishes nested virtualization
    // as a processor feature, which a bare metal size — running the caller's software with no
    // hypervisor under it — does not. It takes no interface attachment from a secondary
    // network, so no private address at all is available on one, and the packets of a session
    // cannot be copied under its guest. The sizes from the `24xlarge` up take their cores all
    // at once, so the one count each of them publishes is its own default. The Elastic Fabric
    // Adapter is offered on the two largest virtualized sizes and on both bare metal ones,
    // which are also offered a trusted platform module and, with it, the UEFI boot mode alone.
    PublishedInstanceTypeFacts::of("c8id.large")
        .ebs(650, 3_600, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .instance_store(1, 118)
        .network(XEON6_BURST_NETWORK_PERFORMANCE)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .ena_srd(false)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .hibernation(false)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8id.xlarge")
        .ebs(1_250, 6_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .instance_store(1, 237)
        .network(XEON6_BURST_NETWORK_PERFORMANCE)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .ena_srd(false)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .hibernation(false)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8id.2xlarge")
        .ebs(2_500, 12_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .instance_store(1, 474)
        .network(XEON6_WIDER_BURST_NETWORK_PERFORMANCE)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .ena_srd(false)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .hibernation(false)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8id.4xlarge")
        .ebs(5_000, 20_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .instance_store(1, 950)
        .network(XEON6_WIDER_BURST_NETWORK_PERFORMANCE)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .ena_srd(false)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .hibernation(false)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8id.8xlarge")
        .ebs(10_000, 40_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .instance_store(1, 1_900)
        .network("15 Gigabit")
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(true)
        .hibernation(false)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8id.12xlarge")
        .ebs(15_000, 60_000, 15_000, 60_000)
        .dedicated_ebs_attachments(32)
        .instance_store(1, 2_850)
        .network("22.5 Gigabit")
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(true)
        .hibernation(false)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8id.16xlarge")
        .ebs(20_000, 80_000, 20_000, 80_000)
        .dedicated_ebs_attachments(48)
        .instance_store(1, XEON6_LOCAL_DEVICE_GB)
        .network("30 Gigabit")
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(true)
        .hibernation(false)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8id.24xlarge")
        .cores_in_steps_of(48)
        .ebs(30_000, 120_000, 30_000, 120_000)
        .dedicated_ebs_attachments(64)
        .instance_store(2, 2_850)
        .network("40 Gigabit")
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(true)
        .hibernation(false)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8id.32xlarge")
        .cores_in_steps_of(64)
        .ebs(40_000, 160_000, 40_000, 160_000)
        // The one step of the limit ladder between the 64 the size below publishes and the 128
        // the size above does whose own published figure is not recorded here yet: the row
        // carries the KIND of the limit, which is the family's, and leaves the count to the
        // derivation rather than asserting a figure it does not have.
        .dedicated_attachment_limit()
        .instance_store(2, XEON6_LOCAL_DEVICE_GB)
        .network("50 Gigabit")
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(true)
        .hibernation(false)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8id.48xlarge")
        .cores_in_steps_of(96)
        .ebs(60_000, 240_000, 60_000, 240_000)
        .dedicated_ebs_attachments(128)
        .instance_store(3, XEON6_LOCAL_DEVICE_GB)
        .network("75 Gigabit")
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .efa(1)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(true)
        .hibernation(false)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8id.96xlarge")
        .cores_in_steps_of(XEON6_WHOLE_HOST_CORES)
        .ebs(80_000, 480_000, 80_000, 480_000)
        .dedicated_ebs_attachments(128)
        .instance_store(6, XEON6_LOCAL_DEVICE_GB)
        .network(XEON6_WHOLE_HOST_NETWORK_PERFORMANCE)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .efa(1)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .processor_features(NESTED_VIRTUALIZATION)
        .ena_srd(true)
        .hibernation(false)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8id.metal-48xl")
        .ebs(60_000, 240_000, 60_000, 240_000)
        .shared_ebs_attachments(39)
        .instance_store(3, XEON6_LOCAL_DEVICE_GB)
        .network("75 Gigabit")
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .efa(1)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .ena_srd(true)
        .nitro_tpm(true)
        .boot_modes(UEFI_BOOT_MODE)
        .hibernation(false)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8id.metal-96xl")
        .ebs(80_000, 480_000, 80_000, 480_000)
        .shared_ebs_attachments(79)
        .instance_store(6, XEON6_LOCAL_DEVICE_GB)
        .network(XEON6_WHOLE_HOST_NETWORK_PERFORMANCE)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .efa(1)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .ena_srd(true)
        .nitro_tpm(true)
        .boot_modes(UEFI_BOOT_MODE)
        .hibernation(false)
        .host_features(false, true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
];

/// "The network performance" the two smallest sizes publish: they burst above their share of
/// the host's bandwidth up to this ceiling.
const XEON6_BURST_NETWORK_PERFORMANCE: &str = "Up to 12.5 Gigabit";

/// The same figure for the two sizes above them, whose adapter bursts to a wider ceiling.
const XEON6_WIDER_BURST_NETWORK_PERFORMANCE: &str = "Up to 15 Gigabit";

/// The same figure for a size that holds the WHOLE host, which is the host's own bandwidth.
const XEON6_WHOLE_HOST_NETWORK_PERFORMANCE: &str = "100 Gigabit";

/// The size, in GB, of ONE local NVMe device of this hardware generation: a store bigger than
/// this is more devices of this size, and a size whose share of the host's storage is below it
/// holds one device of that share.
const XEON6_LOCAL_DEVICE_GB: i64 = 3_800;

/// The cores of the WHOLE host, which the size that holds it takes all at once — so the one
/// core count that size publishes is its default.
const XEON6_WHOLE_HOST_CORES: i32 = 192;
