// SPDX-License-Identifier: CC0-1.0

//! The published rows of the EC2 Mac families, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! An EC2 Mac is a whole Mac computer attached to a Nitro controller, and its only size is
//! `metal`, so the size ladder yields nothing about it: the EBS bandwidth and IOPS the Nitro
//! controller dedicates to it, the Gigabit of its network, how many interfaces it takes and
//! how many addresses each of those takes, the queues one interface is given, the local SSD
//! the newer Macs present to the guest and how many EBS volumes the whole host can hold are
//! FACTS published per Mac host. The vCPUs and the memory of each host are published in
//! [`super::super::MAC_HOSTS`], which the derivation above already reads, so no row restates
//! them.
//!
//! The figures come from the "Amazon EC2 Mac instances" chapter of the EC2 User Guide (the
//! host configurations, their network performance and their local storage), the
//! "Amazon EBS optimized instance types" table (the EBS figures), "Instance volume limits"
//! (the attachment totals) and the "IP addresses per network interface per instance type"
//! table (the interface and address limits).
//!
//! The packets of a session ARE copied on these hosts: the mirroring is done by the Nitro
//! controller the Mac is attached to rather than by a hypervisor under the guest, which is why
//! every row publishes Traffic Mirroring even though each host is a bare metal instance.

use super::*;

/// The published hardware facts of the EC2 Mac hosts, one row per instance type.
pub(super) static MAC_HOST_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // mac1 — the Intel Mac mini host: an 8th generation Intel Core i7 at a sustained 3.2 GHz.
    // Its Nitro controller dedicates 14,000 Mbps and 80,000 IOPS to EBS — a pre-Nitro-era
    // figure neither the size ladder nor the Apple silicon hosts share — and its EBS volumes
    // are limited to 16 attachments, below what the shared Nitro pool would leave.
    PublishedInstanceTypeFacts::of("mac1.metal")
        .ebs(14_000, 80_000, 14_000, 80_000)
        .shared_ebs_attachments(16)
        .network(MAC_NETWORK_PERFORMANCE)
        .network_bandwidth(MAC_NETWORK_GBPS, MAC_NETWORK_GBPS)
        .traffic_mirroring(true)
        .interfaces(MAC_NETWORK_INTERFACES)
        .ipv4_per_interface(MAC_IPV4_ADDRESSES_PER_INTERFACE)
        .ipv4_per_secondary_interface(MAC_IPV4_ADDRESSES_PER_SECONDARY_INTERFACE)
        .processor("Intel", 3.2),
    // mac2 — the Apple M1 host. Every Apple silicon Mac host publishes the same EBS figures
    // (10,000 Mbps and 55,000 IOPS, dedicated rather than burst) and the same network, and the
    // M1 and M2 hosts publish 32 ENA queues per interface.
    PublishedInstanceTypeFacts::of("mac2.metal")
        .ebs(MAC_EBS_MBPS, MAC_EBS_IOPS, MAC_EBS_MBPS, MAC_EBS_IOPS)
        .network(MAC_NETWORK_PERFORMANCE)
        .network_bandwidth(MAC_NETWORK_GBPS, MAC_NETWORK_GBPS)
        .traffic_mirroring(true)
        .interfaces(MAC_NETWORK_INTERFACES)
        .ipv4_per_interface(MAC_IPV4_ADDRESSES_PER_INTERFACE)
        .ipv4_per_secondary_interface(MAC_IPV4_ADDRESSES_PER_SECONDARY_INTERFACE)
        .ena_queues(MAC2_ENA_QUEUES_PER_INTERFACE),
    // mac2-m1ultra — the Apple M1 Ultra host.
    PublishedInstanceTypeFacts::of("mac2-m1ultra.metal")
        .ebs(MAC_EBS_MBPS, MAC_EBS_IOPS, MAC_EBS_MBPS, MAC_EBS_IOPS)
        .network(MAC_NETWORK_PERFORMANCE)
        .network_bandwidth(MAC_NETWORK_GBPS, MAC_NETWORK_GBPS)
        .traffic_mirroring(true)
        .interfaces(MAC_NETWORK_INTERFACES)
        .ipv4_per_interface(MAC_IPV4_ADDRESSES_PER_INTERFACE)
        .ipv4_per_secondary_interface(MAC_IPV4_ADDRESSES_PER_SECONDARY_INTERFACE)
        .ena_queues(MAC2_ENA_QUEUES_PER_INTERFACE),
    // mac2-m2 — the Apple M2 host.
    PublishedInstanceTypeFacts::of("mac2-m2.metal")
        .ebs(MAC_EBS_MBPS, MAC_EBS_IOPS, MAC_EBS_MBPS, MAC_EBS_IOPS)
        .network(MAC_NETWORK_PERFORMANCE)
        .network_bandwidth(MAC_NETWORK_GBPS, MAC_NETWORK_GBPS)
        .traffic_mirroring(true)
        .interfaces(MAC_NETWORK_INTERFACES)
        .ipv4_per_interface(MAC_IPV4_ADDRESSES_PER_INTERFACE)
        .ipv4_per_secondary_interface(MAC_IPV4_ADDRESSES_PER_SECONDARY_INTERFACE)
        .ena_queues(MAC2_ENA_QUEUES_PER_INTERFACE),
    // mac2-m2pro — the Apple M2 Pro host.
    PublishedInstanceTypeFacts::of("mac2-m2pro.metal")
        .ebs(MAC_EBS_MBPS, MAC_EBS_IOPS, MAC_EBS_MBPS, MAC_EBS_IOPS)
        .network(MAC_NETWORK_PERFORMANCE)
        .network_bandwidth(MAC_NETWORK_GBPS, MAC_NETWORK_GBPS)
        .traffic_mirroring(true)
        .interfaces(MAC_NETWORK_INTERFACES)
        .ipv4_per_interface(MAC_IPV4_ADDRESSES_PER_INTERFACE)
        .ipv4_per_secondary_interface(MAC_IPV4_ADDRESSES_PER_SECONDARY_INTERFACE)
        .ena_queues(MAC2_ENA_QUEUES_PER_INTERFACE),
    // mac-m3ultra — the Apple M3 Ultra host, the one Mac host that carries a local Precision
    // Time Protocol hardware clock.
    PublishedInstanceTypeFacts::of("mac-m3ultra.metal")
        .ebs(MAC_EBS_MBPS, MAC_EBS_IOPS, MAC_EBS_MBPS, MAC_EBS_IOPS)
        .network(MAC_NETWORK_PERFORMANCE)
        .network_bandwidth(MAC_NETWORK_GBPS, MAC_NETWORK_GBPS)
        .traffic_mirroring(true)
        .interfaces(MAC_NETWORK_INTERFACES)
        .ipv4_per_interface(MAC_IPV4_ADDRESSES_PER_INTERFACE)
        .ipv4_per_secondary_interface(MAC_IPV4_ADDRESSES_PER_SECONDARY_INTERFACE)
        .host_features(false, true),
    // mac-m4 — the Apple M4 host, which presents its internal 1,900 GB SSD to the guest as a
    // local instance store device and encrypts the traffic between instances in transit. The
    // whole host still holds 27 EBS volumes beside that device.
    PublishedInstanceTypeFacts::of("mac-m4.metal")
        .ebs(MAC_EBS_MBPS, MAC_EBS_IOPS, MAC_EBS_MBPS, MAC_EBS_IOPS)
        .shared_ebs_attachments(MAC_EBS_ATTACHMENTS_WITH_LOCAL_STORE)
        .instance_store(1, MAC_INSTANCE_STORE_GB)
        .network(MAC_NETWORK_PERFORMANCE)
        .network_bandwidth(MAC_NETWORK_GBPS, MAC_NETWORK_GBPS)
        .traffic_mirroring(true)
        .interfaces(MAC_NETWORK_INTERFACES)
        .ipv4_per_interface(MAC_IPV4_ADDRESSES_PER_INTERFACE)
        .ipv4_per_secondary_interface(MAC_IPV4_ADDRESSES_PER_SECONDARY_INTERFACE)
        .encryption_in_transit(true),
    // mac-m4pro — the Apple M4 Pro host, with the same local SSD and in-transit encryption as
    // the M4 one.
    PublishedInstanceTypeFacts::of("mac-m4pro.metal")
        .ebs(MAC_EBS_MBPS, MAC_EBS_IOPS, MAC_EBS_MBPS, MAC_EBS_IOPS)
        .shared_ebs_attachments(MAC_EBS_ATTACHMENTS_WITH_LOCAL_STORE)
        .instance_store(1, MAC_INSTANCE_STORE_GB)
        .network(MAC_NETWORK_PERFORMANCE)
        .network_bandwidth(MAC_NETWORK_GBPS, MAC_NETWORK_GBPS)
        .traffic_mirroring(true)
        .interfaces(MAC_NETWORK_INTERFACES)
        .ipv4_per_interface(MAC_IPV4_ADDRESSES_PER_INTERFACE)
        .ipv4_per_secondary_interface(MAC_IPV4_ADDRESSES_PER_SECONDARY_INTERFACE)
        .encryption_in_transit(true),
    // mac-m4max — the Apple M4 Max host, whose EBS volumes are limited to 10 attachments and
    // whose interfaces are given 8 ENA queues each.
    PublishedInstanceTypeFacts::of("mac-m4max.metal")
        .ebs(MAC_EBS_MBPS, MAC_EBS_IOPS, MAC_EBS_MBPS, MAC_EBS_IOPS)
        .shared_ebs_attachments(10)
        .network(MAC_NETWORK_PERFORMANCE)
        .network_bandwidth(MAC_NETWORK_GBPS, MAC_NETWORK_GBPS)
        .traffic_mirroring(true)
        .interfaces(MAC_NETWORK_INTERFACES)
        .ipv4_per_interface(MAC_IPV4_ADDRESSES_PER_INTERFACE)
        .ipv4_per_secondary_interface(MAC_IPV4_ADDRESSES_PER_SECONDARY_INTERFACE)
        .ena_queues(MAC_M4_ENA_QUEUES_PER_INTERFACE),
];

/// The EBS bandwidth, in Mbps, an Apple silicon Mac host's Nitro controller dedicates to it.
///
/// Dedicated rather than burst, so the figure is both the baseline and the maximum of the
/// published row — a whole host has no share of a larger one to burst into.
const MAC_EBS_MBPS: i32 = 10_000;

/// The EBS I/O operations per second that same controller dedicates to it.
const MAC_EBS_IOPS: i32 = 55_000;

/// "The network performance" of an EC2 Mac host, as the published table writes that column.
const MAC_NETWORK_PERFORMANCE: &str = "10 Gigabit";

/// That same figure in Gbps, which is what the host's one network card carries and reaches.
const MAC_NETWORK_GBPS: f64 = 10.0;

/// The network interfaces one EC2 Mac host takes.
const MAC_NETWORK_INTERFACES: i32 = 8;

/// The private IPv4 addresses each of those interfaces takes.
const MAC_IPV4_ADDRESSES_PER_INTERFACE: i32 = 30;

/// The private IPv4 addresses an interface attached from a secondary network would take: none
/// at all, an EC2 Mac taking no such attachment.
const MAC_IPV4_ADDRESSES_PER_SECONDARY_INTERFACE: i32 = 0;

/// The ENA queues one interface of an M1 or M2 generation Mac host is given by default.
const MAC2_ENA_QUEUES_PER_INTERFACE: i32 = 32;

/// The ENA queues one interface of an M4 generation Mac host is given by default.
const MAC_M4_ENA_QUEUES_PER_INTERFACE: i32 = 8;

/// The local SSD, in GB, an M4 generation Mac host presents to its guest as one device.
const MAC_INSTANCE_STORE_GB: i64 = 1_900;

/// The EBS volumes a Mac host that carries that device can hold attached: the whole host's
/// published total, which its one local device does not come out of.
const MAC_EBS_ATTACHMENTS_WITH_LOCAL_STORE: i32 = 27;
