// SPDX-License-Identifier: CC0-1.0

//! The published rows of the AMD EPYC families, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! An AMD family differs from the Intel and Graviton families beside it in more than its
//! processor: its EBS figures are its own at every size, the cores of an EPYC are configured
//! TWO at a time (`VCpuInfo$ValidCores` publishes the even counts alone), and the newest
//! generation ships with simultaneous multithreading disabled, so each of its vCPUs is a core
//! of its own and the only thread count it can be configured with is one. None of that
//! follows from a size multiplier, which is why these rows exist.
//!
//! Each family's comment says where its figures come from: the family's own instance-type
//! specification page (vCPUs, memory, network performance), the "Amazon EBS optimized
//! instance types" table (the four EBS figures), "Instance volume limits" (how many volumes a
//! type holds attached and whether that limit is the volumes' own) and the "IP addresses per
//! network interface per instance type" table (the interface and address limits).

use super::*;

/// The published hardware facts of the AMD EPYC families' instance types, one row per type.
pub(super) static AMD_EPYC_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // C8a — compute optimized on a 5th generation AMD EPYC at a sustained 4.5 GHz, with no
    // local instance store. The family ships with simultaneous multithreading DISABLED, so
    // every vCPU of a size is a core of its own and the only thread count it publishes is one.
    // Its network and EBS bandwidth is CONFIGURABLE, so
    // every size publishes the bandwidth weightings a launch may ask for, and its connection
    // tracking applies the shorter established-TCP idle timeout the newest hardware publishes.
    // Its EBS figures are the ladder's rows of "Amazon EBS optimized instance types": the
    // sizes below the `8xlarge` publish a baseline they burst above, up to 10,000 Mbps and
    // 40,000 IOPS, and the bandwidth is dedicated from that size up, reaching 40,000 Mbps and
    // 240,000 IOPS on the size that holds the whole host. Those volumes are attached to EBS
    // hardware of the type's own rather than out of the pool shared with its interfaces, so
    // the limit every virtualized size reports is the EBS devices' own and is of the
    // `dedicated` kind — 32 volumes up to the `12xlarge`, then 48, 64 and 128 as the size
    // grows — while the half-host bare metal size reports its own 39 and the whole-host one
    // the 79 its host shares between every device. ENA Express is offered from the `16xlarge`
    // up, the sizes under it carrying less dedicated network bandwidth than its transport
    // needs;
    // it takes no interface attachment from a secondary network (so no private address at all
    // is available on one), the packets of a session cannot be copied under its guest, and the
    // Elastic Fabric Adapter is offered on the size that holds the whole host and on the bare
    // metal size of the same width. A bare metal size of an x86 family boots in the legacy
    // BIOS mode alone.
    PublishedInstanceTypeFacts::of("c8a.medium")
        .threads_per_core(1)
        .ebs(315, 2_500, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("AMD", 4.5)
        .ena_srd(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8a.large")
        .threads_per_core(1)
        .ebs(630, 3_600, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("AMD", 4.5)
        .ena_srd(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8a.xlarge")
        .threads_per_core(1)
        .ebs(1_250, 6_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("AMD", 4.5)
        .ena_srd(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8a.2xlarge")
        .threads_per_core(1)
        .ebs(2_500, 12_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("AMD", 4.5)
        .ena_srd(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8a.4xlarge")
        .threads_per_core(1)
        .ebs(5_000, 20_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("AMD", 4.5)
        .ena_srd(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8a.8xlarge")
        .threads_per_core(1)
        .ebs(10_000, 40_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("AMD", 4.5)
        .ena_srd(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8a.12xlarge")
        .threads_per_core(1)
        .ebs(15_000, 60_000, 15_000, 60_000)
        .dedicated_ebs_attachments(32)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("AMD", 4.5)
        .ena_srd(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8a.16xlarge")
        .threads_per_core(1)
        .ebs(20_000, 80_000, 20_000, 80_000)
        .dedicated_ebs_attachments(48)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("AMD", 4.5)
        .ena_srd(true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8a.24xlarge")
        .threads_per_core(1)
        .ebs(30_000, 120_000, 30_000, 120_000)
        .dedicated_ebs_attachments(64)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("AMD", 4.5)
        .ena_srd(true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8a.48xlarge")
        .threads_per_core(1)
        .ebs(40_000, 240_000, 40_000, 240_000)
        .dedicated_ebs_attachments(128)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .efa(1)
        .processor("AMD", 4.5)
        .ena_srd(true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8a.metal-24xl")
        .threads_per_core(1)
        .ebs(30_000, 120_000, 30_000, 120_000)
        .dedicated_ebs_attachments(39)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("AMD", 4.5)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        .ena_srd(true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("c8a.metal-48xl")
        .threads_per_core(1)
        .ebs(40_000, 240_000, 40_000, 240_000)
        .ebs_attachments(79)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .efa(1)
        .processor("AMD", 4.5)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        .ena_srd(true)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    // R5a — memory optimized on a 1st generation AMD EPYC (7571) at a sustained 2.2 GHz, with
    // no local instance store. The family's EBS figures are its OWN rather than the R5
    // ladder's: the three sizes below the `4xlarge` publish a baseline they burst above, up to
    // that size's dedicated 2,880 Mbps and 16,000 IOPS, and from it the bandwidth is dedicated
    // per size up to 13,570 Mbps and 60,000 IOPS on the size that holds the whole host — well
    // under the figures the Intel ladder of the same generation reaches. Its EBS volumes come
    // out of the pool shared with its interfaces and publish 27 attachments at every size. The
    // family is NOT offered on a Dedicated Host, its hardware generation carries no local PTP
    // clock and does not migrate an instance on a user-initiated reboot, and the cores of an
    // EPYC are configured two at a time, so each size publishes the even core counts alone. Its
    // network figures are its own published ones rather than a share of the Intel host of the
    // same generation: the four narrowest sizes sustain the ladder's baseline and burst to
    // 10 Gigabit, the `8xlarge` sustains the figure published for it and bursts to that same
    // ceiling, and from the `12xlarge` up the figure is DEDICATED, so each of those sizes
    // reports it as the bandwidth it sustains and the bandwidth it reaches alike.
    // Each of its interfaces is given the eight ENA queues its adapter carries, whatever the
    // size.
    PublishedInstanceTypeFacts::of("r5a.large")
        .cores_in_steps_of(2)
        .ebs(650, 3_600, 2_880, 16_000)
        .ebs_attachments(27)
        .network_bandwidth(0.75, 10.0)
        .ena_queues(FIRST_EPYC_ENA_QUEUES_PER_INTERFACE)
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5a.xlarge")
        .cores_in_steps_of(2)
        .ebs(1_085, 6_000, 2_880, 16_000)
        .ebs_attachments(27)
        .network_bandwidth(1.25, 10.0)
        .ena_queues(FIRST_EPYC_ENA_QUEUES_PER_INTERFACE)
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5a.2xlarge")
        .cores_in_steps_of(2)
        .ebs(1_580, 8_333, 2_880, 16_000)
        .ebs_attachments(27)
        .network_bandwidth(2.5, 10.0)
        .ena_queues(FIRST_EPYC_ENA_QUEUES_PER_INTERFACE)
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5a.4xlarge")
        .cores_in_steps_of(2)
        .ebs(2_880, 16_000, 2_880, 16_000)
        .ebs_attachments(27)
        .network_bandwidth(5.0, 10.0)
        .ena_queues(FIRST_EPYC_ENA_QUEUES_PER_INTERFACE)
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5a.8xlarge")
        .cores_in_steps_of(2)
        .ebs(4_750, 20_000, 4_750, 20_000)
        .ebs_attachments(27)
        .network_bandwidth(7.5, 10.0)
        .ena_queues(FIRST_EPYC_ENA_QUEUES_PER_INTERFACE)
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5a.12xlarge")
        .cores_in_steps_of(2)
        .ebs(6_780, 30_000, 6_780, 30_000)
        .ebs_attachments(27)
        .network_bandwidth(10.0, 10.0)
        .ena_queues(FIRST_EPYC_ENA_QUEUES_PER_INTERFACE)
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5a.16xlarge")
        .cores_in_steps_of(2)
        .ebs(9_500, 40_000, 9_500, 40_000)
        .ebs_attachments(27)
        .network_bandwidth(12.0, 12.0)
        .ena_queues(FIRST_EPYC_ENA_QUEUES_PER_INTERFACE)
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5a.24xlarge")
        .cores_in_steps_of(2)
        .ebs(13_570, 60_000, 13_570, 60_000)
        .ebs_attachments(27)
        .network_bandwidth(20.0, 20.0)
        .ena_queues(FIRST_EPYC_ENA_QUEUES_PER_INTERFACE)
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    // T3a — burstable performance on the same 1st generation AMD EPYC (7571) at a sustained
    // 2.2 GHz. Its EBS baseline doubles with the credits each size earns up to the `large` and
    // then stops, so the three largest sizes share one row, and every size bursts above that
    // baseline to the family's ceiling — 2,085 Mbps and 11,800 IOPS below the `large`, then
    // 2,780 Mbps and 15,700 IOPS. Its EBS volumes come out of the pool shared with its
    // interfaces and publish 27 attachments at every size, its whole ladder publishes one
    // network figure, and its hardware carries no local PTP clock and does not migrate an
    // instance on a user-initiated reboot. The cores of an EPYC are configured two at a time.
    // The `large` size takes 12 private addresses per interface rather than the 10 its step of
    // the size ladder publishes.
    PublishedInstanceTypeFacts::of("t3a.nano")
        .cores_in_steps_of(2)
        .ebs(43, 250, 2_085, 11_800)
        .ebs_attachments(27)
        .network("Up to 5 Gigabit")
        .interfaces(2)
        .ipv4_per_interface(2)
        .processor("AMD", 2.2)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("t3a.micro")
        .cores_in_steps_of(2)
        .ebs(87, 500, 2_085, 11_800)
        .ebs_attachments(27)
        .network("Up to 5 Gigabit")
        .interfaces(2)
        .ipv4_per_interface(2)
        .processor("AMD", 2.2)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("t3a.small")
        .cores_in_steps_of(2)
        .ebs(175, 1_000, 2_085, 11_800)
        .ebs_attachments(27)
        .network("Up to 5 Gigabit")
        .ipv4_per_interface(4)
        .processor("AMD", 2.2)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("t3a.medium")
        .cores_in_steps_of(2)
        .ebs(350, 2_000, 2_085, 11_800)
        .ebs_attachments(27)
        .network("Up to 5 Gigabit")
        .ipv4_per_interface(6)
        .processor("AMD", 2.2)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("t3a.large")
        .cores_in_steps_of(2)
        .ebs(695, 4_000, 2_780, 15_700)
        .ebs_attachments(27)
        .network("Up to 5 Gigabit")
        .ipv4_per_interface(12)
        .processor("AMD", 2.2)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("t3a.xlarge")
        .cores_in_steps_of(2)
        .ebs(695, 4_000, 2_780, 15_700)
        .ebs_attachments(27)
        .network("Up to 5 Gigabit")
        .interfaces(4)
        .ipv4_per_interface(15)
        .processor("AMD", 2.2)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("t3a.2xlarge")
        .cores_in_steps_of(2)
        .ebs(695, 4_000, 2_780, 15_700)
        .ebs_attachments(27)
        .network("Up to 5 Gigabit")
        .interfaces(4)
        .ipv4_per_interface(15)
        .processor("AMD", 2.2)
        .host_features(false, false),
    // R5ad — memory optimized on the same 1st generation AMD EPYC (7571) at a sustained
    // 2.2 GHz, with local NVMe devices. Its EBS figures are the R5a ladder's, the two families
    // sharing one hardware generation: the three sizes below the `4xlarge` publish a baseline
    // they burst above, up to that size's dedicated 2,880 Mbps and 16,000 IOPS, and from it the
    // bandwidth is dedicated per size up to 13,570 Mbps and 60,000 IOPS on the size that holds
    // the whole host. What the `d` adds is the local store, published per type as a count and a
    // size that follow no ratio — one device up to the `2xlarge`, two from the `4xlarge` and
    // four from the `16xlarge` — and it is counted against the attachment pool its EBS volumes
    // come out of, so the volume limit each size reports is what those devices leave. The
    // family is NOT offered on a Dedicated Host, its hardware generation carries no local PTP
    // clock and does not migrate an instance on a user-initiated reboot, and the cores of an
    // EPYC are configured two at a time.
    PublishedInstanceTypeFacts::of("r5ad.large")
        .cores_in_steps_of(2)
        .ebs(650, 3_600, 2_880, 16_000)
        .instance_store(1, 75)
        .network("Up to 10 Gigabit")
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5ad.xlarge")
        .cores_in_steps_of(2)
        .ebs(1_085, 6_000, 2_880, 16_000)
        .instance_store(1, 150)
        .network("Up to 10 Gigabit")
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5ad.2xlarge")
        .cores_in_steps_of(2)
        .ebs(1_580, 8_333, 2_880, 16_000)
        .instance_store(1, 300)
        .network("Up to 10 Gigabit")
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5ad.4xlarge")
        .cores_in_steps_of(2)
        .ebs(2_880, 16_000, 2_880, 16_000)
        .instance_store(2, 300)
        .network("Up to 10 Gigabit")
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5ad.8xlarge")
        .cores_in_steps_of(2)
        .ebs(4_750, 20_000, 4_750, 20_000)
        .instance_store(2, 600)
        .network("Up to 10 Gigabit")
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5ad.12xlarge")
        .cores_in_steps_of(2)
        .ebs(6_780, 30_000, 6_780, 30_000)
        .instance_store(2, 900)
        .network("10 Gigabit")
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5ad.16xlarge")
        .cores_in_steps_of(2)
        .ebs(9_500, 40_000, 9_500, 40_000)
        .instance_store(4, 600)
        .network("12 Gigabit")
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5ad.24xlarge")
        .cores_in_steps_of(2)
        .ebs(13_570, 60_000, 13_570, 60_000)
        .instance_store(4, 900)
        .network("20 Gigabit")
        .processor("AMD", 2.2)
        .dedicated_hosts(false)
        .host_features(false, false),
    // M5a — general purpose on the same 1st generation AMD EPYC (7571) at a sustained 2.5 GHz,
    // with no local instance store. Its EBS figures are the family's own rows of "Amazon EBS
    // optimized instance types": the three sizes below the `4xlarge` publish a baseline they
    // burst above, up to that size's dedicated 2,880 Mbps and 16,000 IOPS, and from it the
    // bandwidth is dedicated per size up to 13,750 Mbps and 60,000 IOPS on the size that holds
    // the whole host — well under the figures the Intel ladder of the same generation reaches,
    // and with the baseline and the maximum EQUAL from the `4xlarge` up. Its EBS volumes come
    // out of the pool shared with its interfaces and publish 27 attachments at every size, and
    // its published network column is one figure for the whole ladder up to the `8xlarge` and
    // the size's own share of the host above it. The family is NOT offered on a Dedicated Host.
    // The cores of an EPYC are configured two at a time, so the published core counts of a size
    // are the even ones — from two up to the `4xlarge`, then from the quarter of its cores each
    // wider size publishes as its smallest configuration (four at the `8xlarge`, eight at the
    // `16xlarge`) — except the `12xlarge`, which publishes the multiples of that quarter alone,
    // and the size that holds the whole host, which takes its cores all at once and so publishes
    // the one count its default is. The `xlarge`, with two cores, publishes the one count two.
    PublishedInstanceTypeFacts::of("m5a.large")
        .cores_in_steps_of(2)
        .ebs(650, 3_600, 2_880, 16_000)
        .ebs_attachments(27)
        .network("Up to 10 Gigabit")
        .processor("AMD", 2.5)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("m5a.xlarge")
        .cores_in_steps_of(2)
        .ebs(1_085, 6_000, 2_880, 16_000)
        .ebs_attachments(27)
        .network("Up to 10 Gigabit")
        .processor("AMD", 2.5)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("m5a.2xlarge")
        .cores_in_steps_of(2)
        .ebs(1_580, 8_333, 2_880, 16_000)
        .ebs_attachments(27)
        .network("Up to 10 Gigabit")
        .processor("AMD", 2.5)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("m5a.4xlarge")
        .cores_in_steps_of(2)
        .ebs(2_880, 16_000, 2_880, 16_000)
        .ebs_attachments(27)
        .network("Up to 10 Gigabit")
        .processor("AMD", 2.5)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("m5a.8xlarge")
        .cores_in_steps_of(2)
        .cores_from(4)
        .ebs(4_750, 20_000, 4_750, 20_000)
        .ebs_attachments(27)
        .network("Up to 10 Gigabit")
        .processor("AMD", 2.5)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("m5a.12xlarge")
        .cores_in_steps_of(6)
        .ebs(6_780, 30_000, 6_780, 30_000)
        .ebs_attachments(27)
        .network("10 Gigabit")
        .processor("AMD", 2.5)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("m5a.16xlarge")
        .cores_in_steps_of(2)
        .cores_from(8)
        .ebs(9_500, 40_000, 9_500, 40_000)
        .ebs_attachments(27)
        .network("12 Gigabit")
        .processor("AMD", 2.5)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("m5a.24xlarge")
        .cores_all_at_once()
        .ebs(13_750, 60_000, 13_750, 60_000)
        .ebs_attachments(27)
        .network("20 Gigabit")
        .processor("AMD", 2.5)
        .dedicated_hosts(false),
    // M6a — general purpose on a 3rd generation AMD EPYC (7R13, Milan) at a sustained 3.6 GHz,
    // with no local instance store, on the same host as the compute optimized family of the
    // same generation: a 50 Gigabit, 192 vCPU host whose EBS rows the two families share. Those
    // rows are the family's own of "Amazon EBS optimized instance types": the four sizes below
    // the `8xlarge` publish a baseline they burst above, up to 10,000 Mbps and 40,000 IOPS, and
    // the bandwidth is dedicated from that size up, reaching 40,000 Mbps on the `32xlarge` —
    // where the host's EBS hardware is already at its figure, so the wider sizes publish it
    // too — while the IOPS keep growing to the 240,000 of the sizes that hold the whole host.
    // The network column is the published one: a quarter of the host bursting to 12.5 Gigabit
    // below the `8xlarge`, then the size's own share of the 50. Its volumes come out of the
    // pool shared with its interfaces, so every virtualized size leaves the limit the
    // derivation above counts, while the bare metal size carries the 31 EBS volumes "Instance
    // volume limits" publishes for a bare metal instance. ENA Express is offered from the
    // `12xlarge` up — the sizes under it carry less dedicated network bandwidth than its
    // transport needs — and on the bare metal size; the Elastic Fabric Adapter only on the
    // largest virtualized size and that bare metal one, which hold the whole host's network
    // share. The published core counts of a size up to the `4xlarge` are every one up to its
    // own, the `8xlarge` publishes the even ones from four, and each size from the `12xlarge`
    // up takes its cores ALL at once — so the one count it publishes is its default — while the
    // size that holds the whole host takes any count.
    // That bare metal size boots the legacy BIOS alone — UEFI reaches every x86 size built on
    // the Nitro System except a bare metal one, which has no hypervisor to present it — is
    // offered CloudWatch action based recovery, which the hypervisor-backed rule would deny it,
    // and is migrated to another host on a user-initiated reboot, the move being the HOST's to
    // make rather than the hypervisor's it has none of.
    PublishedInstanceTypeFacts::of("m6a.large")
        .ebs(650, 3_600, 10_000, 40_000)
        .network("Up to 12.5 Gigabit")
        .processor("AMD", MILAN_SUSTAINED_GHZ)
        .ena_srd(false),
    PublishedInstanceTypeFacts::of("m6a.xlarge")
        .ebs(1_250, 6_000, 10_000, 40_000)
        .network("Up to 12.5 Gigabit")
        .processor("AMD", MILAN_SUSTAINED_GHZ)
        .ena_srd(false),
    PublishedInstanceTypeFacts::of("m6a.2xlarge")
        .ebs(2_500, 12_000, 10_000, 40_000)
        .network("Up to 12.5 Gigabit")
        .processor("AMD", MILAN_SUSTAINED_GHZ)
        .ena_srd(false),
    PublishedInstanceTypeFacts::of("m6a.4xlarge")
        .ebs(5_000, 20_000, 10_000, 40_000)
        .network("Up to 12.5 Gigabit")
        .processor("AMD", MILAN_SUSTAINED_GHZ)
        .ena_srd(false),
    PublishedInstanceTypeFacts::of("m6a.8xlarge")
        .cores_in_steps_of(2)
        .cores_from(4)
        .ebs(10_000, 40_000, 10_000, 40_000)
        .network("12.5 Gigabit")
        .processor("AMD", MILAN_SUSTAINED_GHZ)
        .ena_srd(false),
    PublishedInstanceTypeFacts::of("m6a.12xlarge")
        .cores_all_at_once()
        .ebs(15_000, 60_000, 15_000, 60_000)
        .network("18.75 Gigabit")
        .processor("AMD", MILAN_SUSTAINED_GHZ),
    PublishedInstanceTypeFacts::of("m6a.16xlarge")
        .cores_all_at_once()
        .ebs(20_000, 80_000, 20_000, 80_000)
        .network("25 Gigabit")
        .processor("AMD", MILAN_SUSTAINED_GHZ),
    PublishedInstanceTypeFacts::of("m6a.24xlarge")
        .cores_all_at_once()
        .ebs(30_000, 120_000, 30_000, 120_000)
        .network("37.5 Gigabit")
        .processor("AMD", MILAN_SUSTAINED_GHZ),
    PublishedInstanceTypeFacts::of("m6a.32xlarge")
        .cores_all_at_once()
        .ebs(40_000, 160_000, 40_000, 160_000)
        .network("50 Gigabit")
        .processor("AMD", MILAN_SUSTAINED_GHZ),
    PublishedInstanceTypeFacts::of("m6a.48xlarge")
        .cores_all_at_once()
        .ebs(40_000, 240_000, 40_000, 240_000)
        .network("50 Gigabit")
        .efa(1)
        .processor("AMD", MILAN_SUSTAINED_GHZ),
    PublishedInstanceTypeFacts::of("m6a.metal")
        .ebs(40_000, 240_000, 40_000, 240_000)
        .ebs_attachments(31)
        .network("50 Gigabit")
        .efa(1)
        .processor("AMD", MILAN_SUSTAINED_GHZ)
        .ena_srd(true)
        .auto_recovery(true)
        .host_features(true, false)
        .boot_modes(LEGACY_BIOS_BOOT_MODE),
];

/// The ENA queues each interface of an instance on the 1st generation AMD EPYC host is given
/// by default (`NetworkCardInfo$DefaultEnaQueueCountPerInterface`): the queue count of the
/// adapter that host's network card carries, which every size built on it publishes whatever
/// its vCPUs.
const FIRST_EPYC_ENA_QUEUES_PER_INTERFACE: i32 = 8;

/// The sustained frequency, in GHz, of the 3rd generation AMD EPYC host: the all-core turbo
/// its specification page publishes, which is what `ProcessorInfo$SustainedClockSpeedInGhz`
/// reports for every size built on it.
const MILAN_SUSTAINED_GHZ: f64 = 3.6;
