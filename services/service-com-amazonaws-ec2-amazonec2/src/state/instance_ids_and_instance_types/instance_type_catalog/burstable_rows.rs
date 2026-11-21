// SPDX-License-Identifier: CC0-1.0

//! The published rows of the BURSTABLE performance families, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! A burstable family's hardware is not a function of its size multiplier any more than any
//! other family's, and it diverges from the derivation in its own way: every size of a
//! burstable family shares ONE host, so every size publishes the same "Up to" network figure
//! rather than a share that grows with the size, its EBS baseline is the share of the host's
//! sustained figure its size carries while the ceiling it bursts to is the host's, and the
//! `large` size of the T ladder takes TWELVE private addresses per interface where the
//! generic ladder's `large` step takes ten.
//!
//! Each family's comment says where its figures come from: the family's own instance-type
//! specification page (vCPUs, memory, network performance), the "Amazon EBS optimized
//! instance types" table (the four EBS figures), "Instance volume limits" (how many volumes a
//! type holds attached and whether that limit is the volumes' own) and the "IP addresses per
//! network interface per instance type" table (the interface and address limits).

use super::*;

/// The published hardware facts of the burstable families' instance types, one row per type.
pub(super) static BURSTABLE_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // T4g — burstable performance on AWS Graviton2, at a sustained 2.5 GHz, with no local
    // instance store and no accelerator. Every size of the family sits on ONE host and shares
    // its adapter, so each publishes the same "Up to 5 Gigabit" rather than a share of the
    // host that grows with the size. Its EBS figures are the family's rows of "Amazon EBS
    // optimized instance types": the baseline is the size's share of the host's sustained
    // 2,780 Mbps and 11,800 IOPS — halving at each step down the ladder — and every size
    // bursts above it, to 2,085 Mbps and 11,800 IOPS on the sizes up to the `medium` and to
    // 2,780 Mbps and 15,700 IOPS from the `large` up. The `large` size takes twelve private
    // IPv4 addresses per interface, where the generic ladder's `large` step takes ten; the
    // other sizes' interface and address limits are the ladder's own. The volumes a size holds
    // attached come out of the pool it shares with its interfaces, which is what the
    // derivation above already counts, so no row publishes a figure for it.
    PublishedInstanceTypeFacts::of("t4g.nano")
        .ebs(43, 250, 2_085, 11_800)
        .network(BURSTABLE_GRAVITON_NETWORK_PERFORMANCE)
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ),
    PublishedInstanceTypeFacts::of("t4g.micro")
        .ebs(87, 500, 2_085, 11_800)
        .network(BURSTABLE_GRAVITON_NETWORK_PERFORMANCE)
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ),
    PublishedInstanceTypeFacts::of("t4g.small")
        .ebs(174, 1_000, 2_085, 11_800)
        .network(BURSTABLE_GRAVITON_NETWORK_PERFORMANCE)
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ),
    PublishedInstanceTypeFacts::of("t4g.medium")
        .ebs(347, 2_000, 2_085, 11_800)
        .network(BURSTABLE_GRAVITON_NETWORK_PERFORMANCE)
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ),
    PublishedInstanceTypeFacts::of("t4g.large")
        .ebs(695, 4_000, 2_780, 15_700)
        .network(BURSTABLE_GRAVITON_NETWORK_PERFORMANCE)
        .ipv4_per_interface(BURSTABLE_LARGE_IPV4_ADDRESSES)
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ),
    PublishedInstanceTypeFacts::of("t4g.xlarge")
        .ebs(1_390, 6_000, 2_780, 15_700)
        .network(BURSTABLE_GRAVITON_NETWORK_PERFORMANCE)
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ),
    PublishedInstanceTypeFacts::of("t4g.2xlarge")
        .ebs(2_780, 11_800, 2_780, 15_700)
        .network(BURSTABLE_GRAVITON_NETWORK_PERFORMANCE)
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ),
    // T8i — burstable performance on the Intel Xeon 6 hardware the eighth generation Intel
    // families are built on, with no local instance store. Like T4g above, every size shares
    // one host's adapter and publishes the same "Up to" figure, and the EBS baseline halves at
    // each step down the ladder from the host's sustained figure while the ceiling it bursts
    // to — 5,000 Mbps and 20,000 IOPS — is the host's and is the same at every size. What
    // separates it from the earlier burstable families is where its volumes are attached: they
    // hang off EBS hardware of the type's own rather than out of the pool shared with the
    // interfaces, so every size publishes a limit of the `dedicated` kind, and that limit is
    // 32 volumes at every size rather than what a shared pool leaves. Its connection tracking
    // applies this hardware generation's own, much shorter established-TCP idle timeout, it
    // takes no interface attachment from a secondary network — so no private address at all is
    // available on one — the packets of a session cannot be copied under its guest, and ENA
    // Express is not offered: its transport needs more dedicated network bandwidth than a
    // burstable size carries. Its processor is the same Intel Xeon 6 the generation's compute
    // families are built on, so every size publishes that processor's sustained frequency; the
    // additional processor FEATURES are a separate fact and this family publishes none, which
    // is already what an Intel type's derivation answers, so no row states them.
    PublishedInstanceTypeFacts::of("t8i.nano")
        .ebs(43, 375, 5_000, 20_000)
        .dedicated_ebs_attachments(BURSTABLE_DEDICATED_EBS_ATTACHMENTS)
        .network(BURSTABLE_INTEL_NETWORK_PERFORMANCE)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .ena_srd(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("t8i.micro")
        .ebs(87, 750, 5_000, 20_000)
        .dedicated_ebs_attachments(BURSTABLE_DEDICATED_EBS_ATTACHMENTS)
        .network(BURSTABLE_INTEL_NETWORK_PERFORMANCE)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .ena_srd(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("t8i.small")
        .ebs(174, 1_500, 5_000, 20_000)
        .dedicated_ebs_attachments(BURSTABLE_DEDICATED_EBS_ATTACHMENTS)
        .network(BURSTABLE_INTEL_NETWORK_PERFORMANCE)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .ena_srd(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("t8i.medium")
        .ebs(347, 3_000, 5_000, 20_000)
        .dedicated_ebs_attachments(BURSTABLE_DEDICATED_EBS_ATTACHMENTS)
        .network(BURSTABLE_INTEL_NETWORK_PERFORMANCE)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", XEON6_SUSTAINED_GHZ)
        .ena_srd(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .traffic_mirroring(false),
];

/// "The network performance" every size of the Graviton2 burstable family publishes: the
/// sizes share one host's adapter, so the figure is the same at every step of the ladder.
const BURSTABLE_GRAVITON_NETWORK_PERFORMANCE: &str = "Up to 5 Gigabit";

/// The same figure for the Intel Xeon 6 burstable family, whose adapter is the one its
/// hardware generation's compute sizes carry.
const BURSTABLE_INTEL_NETWORK_PERFORMANCE: &str = "Up to 12.5 Gigabit";

/// The sustained all-core frequency, in GHz, of the AWS Graviton2 processor the fourth
/// generation Graviton families are built on.
const GRAVITON2_SUSTAINED_GHZ: f64 = 2.5;

/// The private IPv4 addresses one interface of a burstable `large` size takes
/// (`NetworkInfo$Ipv4AddressesPerInterface`): the T ladder's own figure for that step, which
/// is above the one the generic ladder publishes for a `large`.
const BURSTABLE_LARGE_IPV4_ADDRESSES: i32 = 12;

/// The EBS volumes every size of the Intel Xeon 6 burstable family holds attached: a limit of
/// the volumes' OWN, the same figure at every size rather than what a shared pool leaves.
const BURSTABLE_DEDICATED_EBS_ATTACHMENTS: i32 = 32;
