// SPDX-License-Identifier: CC0-1.0

//! The published rows of the previous generation families, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! A previous generation family is not a smaller version of a current one: its memory is the
//! figure its own specification states rather than a whole number of GiB per vCPU, its local
//! devices are the ones it shipped with, EBS optimization is an OPTION of the launch on the
//! sizes offered it and absent on the rest, its EBS volumes share one attachment total with
//! its interfaces and those devices, and IPv6 reaches only the few previous generation
//! families AWS names beside the current ones. The size multiplier in the name yields none of
//! that, which is why these rows exist.
//!
//! The figures come from the "Previous generation instances" page and the family's own
//! specification (vCPUs, memory, instance store devices, network performance), the
//! "Amazon EBS optimized instance types" table (the EBS figures of the sizes offered the
//! option), "Instance volume limits" (the shared attachment total), the "IP addresses per
//! network interface per instance type" table (the interface and address limits), and the
//! pages that name the types offered a cluster placement group and Traffic Mirroring. The two
//! network bandwidth figures beside each family's "Network performance" words are the host
//! adapter's ([`XEN_ADAPTER_PEAK_GBPS`]) and the size's own step of the published baseline
//! ladder, held to what one instance of such a host sustains
//! ([`XEN_SUSTAINED_CEILING_GBPS`]).

use super::*;

/// The published hardware facts of the previous generation families' instance types, one row
/// per type.
pub(super) static PREVIOUS_GENERATION_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // M3 — general purpose of a PREVIOUS generation, Intel Xeon E5-2670 v2 (Ivy Bridge) at a
    // sustained 2.5 GHz on the Xen hypervisor, with local SATA SSD devices: one device up to
    // the `large` and two from the `xlarge` up, each of a size of its own. Its memory is the
    // family's published 3.75 GiB per vCPU, which is not a whole number of GiB at any size.
    // EBS optimization is an OPTION of the launch on the two larger sizes, each of which
    // publishes ONE fixed figure — a pre-Nitro instance has no baseline it bursts above — and
    // is not offered at all on the `medium` and the `large`, which therefore have no
    // EBS-optimized performance to report. The EBS volumes share their 39 attachments with the
    // interfaces and the local devices. The network figures are the published words of the
    // "Network performance" column rather than a figure in Gigabit, and the smallest size
    // takes two interfaces where its step of the size ladder publishes three. Dedicated Hosts
    // and CloudWatch action based recovery are offered on the family, and an instance is
    // migrated on a user-initiated reboot. IPv6 reaches the C3, R3 and I2 previous generation
    // families alone (`IPV6_PREVIOUS_GENERATION_FAMILIES` holds that list), so no size of this
    // one is offered an IPv6 address, while Amazon EBS
    // encryption — which that same page offers M3 beside those three — is supported on every
    // size.
    PublishedInstanceTypeFacts::of("m3.medium")
        .memory_mib(3_840)
        .ebs_optimized(EBS_OPTIMIZED_UNSUPPORTED)
        .shared_ebs_attachments(39)
        .instance_store(1, 4)
        .network("Moderate")
        .network_bandwidth(0.5, XEN_ADAPTER_PEAK_GBPS)
        .interfaces(2)
        .processor("Intel", 2.5)
        .dedicated_hosts(true)
        .auto_recovery(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m3.large")
        .memory_mib(7_680)
        .ebs_optimized(EBS_OPTIMIZED_UNSUPPORTED)
        .shared_ebs_attachments(39)
        .instance_store(1, 32)
        .network("Moderate")
        .network_bandwidth(0.75, XEN_ADAPTER_PEAK_GBPS)
        .processor("Intel", 2.5)
        .dedicated_hosts(true)
        .auto_recovery(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m3.xlarge")
        .memory_mib(15_360)
        .ebs(500, 4_000, 500, 4_000)
        .shared_ebs_attachments(39)
        .instance_store(2, 40)
        .network("High")
        .network_bandwidth(1.25, XEN_ADAPTER_PEAK_GBPS)
        .processor("Intel", 2.5)
        .dedicated_hosts(true)
        .auto_recovery(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m3.2xlarge")
        .memory_mib(30_720)
        .ebs(1_000, 8_000, 1_000, 8_000)
        .shared_ebs_attachments(39)
        .instance_store(2, 80)
        .network("High")
        .network_bandwidth(2.5, XEN_ADAPTER_PEAK_GBPS)
        .ipv4_per_interface(30)
        .processor("Intel", 2.5)
        .dedicated_hosts(true)
        .auto_recovery(true)
        .host_features(true, false),
    // C3 — compute optimized of a PREVIOUS generation, Intel Xeon E5-2680 v2 (Ivy Bridge) at a
    // sustained 2.8 GHz on the Xen hypervisor, with local SATA SSD devices: TWO of them at every
    // size, each of a size of its own, so the store of a size is twice its device rather than one
    // device of the whole total. Its memory is the family's published 1.875 GiB per vCPU, which
    // is not a whole number of GiB at any size. EBS optimization is an OPTION of the launch on
    // the three middle sizes, each publishing ONE fixed figure — a pre-Nitro instance has no
    // baseline it bursts above — and is offered neither on the smallest size nor on the one that
    // holds the whole host, which therefore have no EBS-optimized performance to report at all.
    // The EBS volumes share their 39 attachments with the interfaces and the local devices. The
    // network figures are the published words of the "Network performance" column rather than a
    // figure in Gigabit. Dedicated Hosts and CloudWatch action based recovery are offered on the
    // family, and it is offered every placement group strategy — a cluster group among them,
    // which the sizes of a family whose share of a host is not a fixed allocation are not. The
    // packets of a session are copied by Nitro hardware these hosts do not have, so Traffic
    // Mirroring is not offered. The size that holds the whole host carries its cores on TWO
    // processor sockets, taken one per socket, so its published core counts go up in twos.
    PublishedInstanceTypeFacts::of("c3.large")
        .memory_mib(3_840)
        .ebs_optimized(EBS_OPTIMIZED_UNSUPPORTED)
        .shared_ebs_attachments(39)
        .instance_store(2, 16)
        .network("Moderate")
        .network_bandwidth(0.75, XEN_ADAPTER_PEAK_GBPS)
        .processor("Intel", 2.8)
        .dedicated_hosts(true)
        .auto_recovery(true)
        .placement_strategies(EVERY_PLACEMENT_STRATEGY)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c3.xlarge")
        .memory_mib(7_680)
        .ebs(500, 4_000, 500, 4_000)
        .shared_ebs_attachments(39)
        .instance_store(2, 40)
        .network("Moderate")
        .network_bandwidth(1.25, XEN_ADAPTER_PEAK_GBPS)
        .processor("Intel", 2.8)
        .dedicated_hosts(true)
        .auto_recovery(true)
        .placement_strategies(EVERY_PLACEMENT_STRATEGY)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c3.2xlarge")
        .memory_mib(15_360)
        .ebs(1_000, 8_000, 1_000, 8_000)
        .shared_ebs_attachments(39)
        .instance_store(2, 80)
        .network("High")
        .network_bandwidth(2.5, XEN_ADAPTER_PEAK_GBPS)
        .processor("Intel", 2.8)
        .dedicated_hosts(true)
        .auto_recovery(true)
        .placement_strategies(EVERY_PLACEMENT_STRATEGY)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c3.4xlarge")
        .memory_mib(30_720)
        .ebs(2_000, 16_000, 2_000, 16_000)
        .shared_ebs_attachments(39)
        .instance_store(2, 160)
        .network("High")
        .network_bandwidth(XEN_SUSTAINED_CEILING_GBPS, XEN_ADAPTER_PEAK_GBPS)
        .processor("Intel", 2.8)
        .dedicated_hosts(true)
        .auto_recovery(true)
        .placement_strategies(EVERY_PLACEMENT_STRATEGY)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c3.8xlarge")
        .memory_mib(61_440)
        .cores_in_steps_of(2)
        .ebs_optimized(EBS_OPTIMIZED_UNSUPPORTED)
        .shared_ebs_attachments(39)
        .instance_store(2, 320)
        .network("10 Gigabit")
        .network_bandwidth(XEN_SUSTAINED_CEILING_GBPS, XEN_ADAPTER_PEAK_GBPS)
        .processor("Intel", 2.8)
        .dedicated_hosts(true)
        .auto_recovery(true)
        .placement_strategies(EVERY_PLACEMENT_STRATEGY)
        .traffic_mirroring(false),
    // C1 — compute optimized of the FIRST generation, on the Xen hypervisor with local
    // magnetic devices: one 350 GB device on the smaller size and four 420 GB ones on the
    // larger. Its memory is the family's published 1.7 GiB and 7 GiB, neither of which is the
    // generic ladder's whole number of GiB per vCPU. EBS optimization is an OPTION of the
    // launch on the larger size, which publishes ONE fixed figure — a pre-Nitro instance has
    // no baseline it bursts above — and is not offered on the smaller one at all, which
    // therefore has no EBS-optimized performance to report. The EBS volumes share their 39
    // attachments with the interfaces and the local devices. The network figures are the
    // published words of the "Network performance" column, with the sustained share and the
    // ceiling of the host's adapter beside them, and the smaller size takes TWO interfaces
    // with six private addresses each where that step of the generic ladder publishes three
    // interfaces and ten addresses. The packets of a session are copied by Nitro hardware
    // these hosts do not have, so Traffic Mirroring is not offered; IPv6 reaches none of this
    // family's sizes, which is already what a previous generation family outside
    // `IPV6_PREVIOUS_GENERATION_FAMILIES` is answered.
    PublishedInstanceTypeFacts::of("c1.medium")
        .memory_mib(1_740)
        .ebs_optimized(EBS_OPTIMIZED_UNSUPPORTED)
        .shared_ebs_attachments(39)
        .instance_store(1, 350)
        .network("Moderate")
        .network_bandwidth(0.5, XEN_ADAPTER_PEAK_GBPS)
        .interfaces(2)
        .ipv4_per_interface(6)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c1.xlarge")
        .memory_mib(7_168)
        .ebs(1_000, 8_000, 1_000, 8_000)
        .shared_ebs_attachments(39)
        .instance_store(4, 420)
        .network("High")
        .network_bandwidth(1.25, XEN_ADAPTER_PEAK_GBPS)
        .interfaces(4)
        .ipv4_per_interface(15)
        .traffic_mirroring(false),
];

/// The Gigabit the network adapter of a pre-Nitro Xen host carries, which is the figure every
/// size carved out of one can reach (`NetworkCardInfo$PeakBandwidthInGbps`).
///
/// The sustained figure beside it (`$BaselineBandwidthInGbps`) is the size's own step of the
/// published baseline ladder — half a Gigabit at the smallest size, then 0.75, 1.25, 2.5 and
/// 5 — held to [`XEN_SUSTAINED_CEILING_GBPS`], which is as much of the adapter as one instance
/// of such a host sustains. That is why the published words of the "Network performance"
/// column (`Moderate`, `High`, `10 Gigabit`) and these two figures are all stated per row: no
/// reading of the size multiplier yields any of them.
const XEN_ADAPTER_PEAK_GBPS: f64 = 10.0;

/// The most of that adapter one instance sustains, whatever its size: the figure the largest
/// sizes of these families publish, which the ladder stops at rather than going on doubling.
const XEN_SUSTAINED_CEILING_GBPS: f64 = 5.0;
