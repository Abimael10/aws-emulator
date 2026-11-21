// SPDX-License-Identifier: CC0-1.0

//! The published rows of the eighth generation Intel network optimized compute family and of
//! the Ice Lake high-memory family with local NVMe devices — C8in and X2idn — read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! Both carry the `n` capability letter, "network and EBS optimized" (the instance type naming
//! conventions), and what it buys is hardware of the family's own: a network share per size
//! that no general purpose ladder yields, EBS figures of its own rows of "Amazon EBS optimized
//! instance types", and — on the sizes that hold a whole host — several Elastic Fabric
//! Adapters. None of that follows from the size multiplier in the name, which is why these
//! rows exist.
//!
//! Each family's comment says where its figures come from: the family's own instance-type
//! specification page (vCPUs, memory, local devices, network performance), the "Amazon EBS
//! optimized instance types" table (the four EBS figures), "Instance volume limits" (how many
//! volumes a type holds attached and whether that limit is the volumes' own) and the "IP
//! addresses per network interface per instance type" table (the interface and address
//! limits).

use super::*;

/// The published hardware facts of the two families' instance types, one row per type.
pub(super) static NETWORK_OPTIMIZED_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // C8in — compute optimized built for the network, on the 8th generation Intel hardware at
    // a sustained 3.9 GHz, with no local instance store. The family's host carries 600 Gigabit
    // of network, each size taking its published share of it — 6.25 Gigabit per `xlarge` of
    // the ladder, with the four sizes below the `8xlarge` sustaining that share and bursting
    // to a published ceiling above it — and its EBS figures are the 1,250 Mbps and 5,000 IOPS
    // per `xlarge` of its rows of "Amazon EBS optimized instance types": a baseline the sizes
    // below the `8xlarge` burst above, up to 10,000 Mbps and 40,000 IOPS, then dedicated
    // bandwidth from that size up to the whole host's 120,000 Mbps and 480,000 IOPS, which the
    // widest size spreads over two EBS cards. Those volumes are attached to EBS hardware of
    // the type's own rather than out of the pool shared with its interfaces, so every
    // virtualized size reports the `dedicated` kind of limit — 32 volumes up to the
    // `12xlarge`, then 48, 64 and the published ceiling of 128 as the size grows, the
    // `32xlarge` reporting the kind alone until its published figure is recorded here — while
    // a bare metal size shares its host's pool with every other device and reports the 31
    // volumes that pool leaves. Its connection tracking applies the shorter established-TCP
    // idle timeout the newest hardware publishes, ENA Express is offered from the size that
    // carries the family's whole dedicated network share up, the sizes under it carrying less
    // than its transport needs, and the Elastic Fabric Adapter only on the sizes that hold a
    // whole host — one on the half-host width and two on the full one. The family takes no
    // interface attachment from a secondary network, so no private address at all is available
    // on one; the packets of a session cannot be copied under its guest and it carries no local
    // PTP hardware clock, while reboot migration is offered on it. Its cores sit on two
    // processor sockets from the `24xlarge` up, so those sizes publish the even counts from
    // four alone — except the `48xlarge`, which holds a whole socket and takes its cores all
    // at once, so the one count it publishes is its default.
    // Each bare metal size is offered CloudWatch action based recovery and reboot migration
    // — the move to another host being the HOST's to make and not the hypervisor's it has
    // none of — the legacy BIOS boot mode alone, and ENA Express, which its adapter carries
    // whether or not a hypervisor sits under the guest.
    PublishedInstanceTypeFacts::of("c8in.large")
        .ebs(650, 3_600, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .network_bandwidth(3.125, 25.0)
        .efa(0)
        .ena_srd(false)
        .processor("Intel", 3.9)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8in.xlarge")
        .ebs(1_250, 6_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .network_bandwidth(6.25, 30.0)
        .efa(0)
        .ena_srd(false)
        .processor("Intel", 3.9)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8in.2xlarge")
        .ebs(2_500, 12_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .network_bandwidth(12.5, 40.0)
        .efa(0)
        .ena_srd(false)
        .processor("Intel", 3.9)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8in.4xlarge")
        .ebs(5_000, 20_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .network_bandwidth(25.0, 50.0)
        .efa(0)
        .ena_srd(false)
        .processor("Intel", 3.9)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8in.8xlarge")
        .ebs(10_000, 40_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .network_bandwidth(50.0, 50.0)
        .efa(0)
        .processor("Intel", 3.9)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8in.12xlarge")
        .ebs(15_000, 60_000, 15_000, 60_000)
        .dedicated_ebs_attachments(32)
        .network_bandwidth(75.0, 75.0)
        .efa(0)
        .processor("Intel", 3.9)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8in.16xlarge")
        .ebs(20_000, 80_000, 20_000, 80_000)
        .dedicated_ebs_attachments(48)
        .network_bandwidth(100.0, 100.0)
        .efa(0)
        .processor("Intel", 3.9)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8in.24xlarge")
        .cores_in_steps_of(2)
        .cores_from(4)
        .ebs(30_000, 120_000, 30_000, 120_000)
        .dedicated_ebs_attachments(64)
        .network_bandwidth(150.0, 150.0)
        .efa(0)
        .processor("Intel", 3.9)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8in.32xlarge")
        .cores_in_steps_of(2)
        .cores_from(4)
        .ebs(40_000, 160_000, 40_000, 160_000)
        .dedicated_attachment_limit()
        .network_bandwidth(200.0, 200.0)
        .efa(0)
        .processor("Intel", 3.9)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8in.48xlarge")
        .cores_all_at_once()
        .ebs(60_000, 240_000, 60_000, 240_000)
        .dedicated_ebs_attachments(128)
        .network_bandwidth(300.0, 300.0)
        .efa(1)
        .processor("Intel", 3.9)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8in.96xlarge")
        .cores_in_steps_of(2)
        .cores_from(4)
        .ebs(120_000, 480_000, 120_000, 480_000)
        .dedicated_ebs_attachments(128)
        .ebs_cards(2)
        .network_bandwidth(600.0, 600.0)
        .efa(2)
        .processor("Intel", 3.9)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8in.metal-48xl")
        .ebs(60_000, 240_000, 60_000, 240_000)
        .ebs_attachments(31)
        .network_bandwidth(300.0, 300.0)
        .efa(1)
        .ena_srd(true)
        .processor("Intel", 3.9)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .auto_recovery(true)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8in.metal-96xl")
        .ebs(120_000, 480_000, 120_000, 480_000)
        .ebs_attachments(31)
        .ebs_cards(2)
        .network_bandwidth(600.0, 600.0)
        .efa(2)
        .ena_srd(true)
        .processor("Intel", 3.9)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .auto_recovery(true)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    // X2idn — memory optimized for in-memory databases on the same 3rd generation Intel Xeon
    // Scalable (Ice Lake) host as the X2iedn family beside it and at the same sustained 3.5
    // GHz, with 16 GiB of memory per vCPU and local NVMe devices: the host's two 1,900 GB
    // devices, of which the narrowest size holds one, while the `24xlarge` holds two smaller
    // 1,425 GB ones. Its EBS bandwidth is dedicated at every published size, 2,500 Mbps per
    // `xlarge` of the ladder, and its IOPS figure is the share of the family's published
    // 260,000 ceiling that the size's bandwidth carries, which that ceiling caps from the
    // `24xlarge` up. The volumes come out of the pool the type shares with its interfaces and
    // its local devices, so the limit each size reports is what that pool leaves, and a bare
    // metal size reports the 31 volumes its host's own pool leaves. CloudWatch action based
    // recovery is offered at every size, the bare metal one included, ENA Express on the whole
    // family — every published size carrying the family's full network share — and the Elastic
    // Fabric Adapter only on the sizes that hold the whole host. Neither reboot migration nor a
    // local PTP hardware clock is offered on this hardware generation. The cores of the
    // virtualized sizes sit on both of the host's processor sockets and are taken one per
    // socket, so those sizes publish the even counts alone while the bare metal one publishes
    // every count, and that bare metal size is offered the legacy BIOS boot mode alone.
    PublishedInstanceTypeFacts::of("x2idn.16xlarge")
        .memory_gib(1_024)
        .cores_in_steps_of(2)
        .ebs(40_000, 173_333, 40_000, 173_333)
        .ebs_attachments(26)
        .instance_store(1, 1_900)
        .network_bandwidth(50.0, 50.0)
        .efa(0)
        .processor("Intel", 3.5)
        .auto_recovery(true)
        .ena_srd(true)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("x2idn.24xlarge")
        .memory_gib(1_536)
        .cores_in_steps_of(2)
        .ebs(60_000, 260_000, 60_000, 260_000)
        .ebs_attachments(25)
        .instance_store(2, 1_425)
        .network_bandwidth(75.0, 75.0)
        .efa(0)
        .processor("Intel", 3.5)
        .auto_recovery(true)
        .ena_srd(true)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("x2idn.32xlarge")
        .memory_gib(2_048)
        .cores_in_steps_of(2)
        .ebs(80_000, 260_000, 80_000, 260_000)
        .ebs_attachments(25)
        .instance_store(2, 1_900)
        .network_bandwidth(100.0, 100.0)
        .efa(1)
        .processor("Intel", 3.5)
        .auto_recovery(true)
        .ena_srd(true)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("x2idn.metal")
        .memory_gib(2_048)
        .ebs(80_000, 260_000, 80_000, 260_000)
        .ebs_attachments(31)
        .instance_store(2, 1_900)
        .network_bandwidth(100.0, 100.0)
        .efa(1)
        .processor("Intel", 3.5)
        .auto_recovery(true)
        .ena_srd(true)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        .host_features(false, false),
];
