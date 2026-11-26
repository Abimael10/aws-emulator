// SPDX-License-Identifier: CC0-1.0

//! The published rows of the burstable performance families that run on the Xen hypervisor,
//! read through [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! The T2 family is a current generation family on PRE-Nitro hardware, which is a combination
//! no rule of the derivation above covers: it has no EBS-optimized mode at all, its EBS
//! volumes share one attachment total of their own with its other devices, it predates the
//! Elastic Network Adapter, its processor publishes the frequency its Turbo reaches rather
//! than the one its generation number would give, its vCPUs run ONE thread per core at every
//! size, and its network performance is the published words of the "Network performance"
//! column rather than a figure in Gigabit. The size multiplier in the name yields none of
//! that.
//!
//! The T1 family before it is a previous generation family whose ONE size predates the CPU
//! credit model the burstable member reports, so AWS publishes it as not a burstable
//! performance type at all, with a memory figure that is no whole number of GiB, a network
//! column of its own, Amazon EBS encryption its generation would not give it, EBS-backed AMIs
//! alone, and an EBS volume limit shared with its other devices.
//!
//! The figures come from the families' own specification ("Burstable performance instances"
//! and the T2 specification table: the network performance words and the processor's
//! frequency), the "Previous generation instances" page (the T1 size's figures),
//! "Instance volume limits" (the shared attachment total), the published list of
//! the types offered enhanced networking with ENA, which does not name either family, and the
//! "IP addresses per network interface per instance type" table (the interface and address
//! limits of the sizes whose row is not the one their step of the size ladder publishes).

use super::*;

/// The published hardware facts of the burstable Xen families' instance types, one row per
/// type.
pub(super) static BURSTABLE_XEN_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // T1 — the FIRST burstable family's one size, on the Xen hypervisor. It predates the CPU
    // credit model `BurstablePerformanceSupported` reports, so AWS publishes it as not a
    // burstable performance type; its published memory is 627 MiB, which is no whole number of
    // GiB; its "Network performance" column reads `Very Low`, and the two figures that column
    // stands for are its own rather than a share of a later host; Amazon EBS encryption IS
    // supported on it, which its generation number alone would not give; its AMIs are
    // EBS-backed ones alone; and its EBS volumes share their limit with its other devices.
    PublishedInstanceTypeFacts::of("t1.micro")
        .burstable_performance(false)
        .memory_mib(627)
        .ebs_encryption(true)
        .shared_attachment_limit()
        .root_devices(EBS_ROOT_DEVICE)
        .network("Very Low")
        .network_bandwidth(T1_BASELINE_GBPS, T1_PEAK_GBPS),
    // T2 — burstable performance on an Intel Xeon whose Turbo reaches a sustained 3.3 GHz, on
    // the Xen hypervisor. Every size reports: no EBS-optimized mode (so no EBS-optimized
    // performance either), 40 EBS volumes shared with its interfaces, no ENA, one thread per
    // core — a T2 vCPU IS a core, so each size publishes its vCPUs as that many cores — and an
    // instance that is migrated on a user-initiated reboot while carrying no local PTP
    // hardware clock. The `large` size takes 12 private addresses per interface rather than the
    // 10 its step of the size ladder publishes, and the two largest sizes take three
    // interfaces rather than four.
    PublishedInstanceTypeFacts::of("t2.nano")
        .threads_per_core(1)
        .shared_ebs_attachments(40)
        .network("Low")
        .network_bandwidth(0.032, BURSTABLE_XEN_PEAK_GBPS)
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 3.3)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("t2.micro")
        .threads_per_core(1)
        .shared_ebs_attachments(40)
        .network("Low to Moderate")
        .network_bandwidth(0.064, BURSTABLE_XEN_PEAK_GBPS)
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 3.3)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("t2.small")
        .threads_per_core(1)
        .shared_ebs_attachments(40)
        .network("Low to Moderate")
        .network_bandwidth(0.128, BURSTABLE_XEN_PEAK_GBPS)
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 3.3)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("t2.medium")
        .threads_per_core(1)
        .shared_ebs_attachments(40)
        .network("Low to Moderate")
        .network_bandwidth(0.256, BURSTABLE_XEN_PEAK_GBPS)
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 3.3)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("t2.large")
        .threads_per_core(1)
        .shared_ebs_attachments(40)
        .network("Low to Moderate")
        .network_bandwidth(0.512, BURSTABLE_XEN_PEAK_GBPS)
        .ipv4_per_interface(12)
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 3.3)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("t2.xlarge")
        .threads_per_core(1)
        .shared_ebs_attachments(40)
        .network("Moderate")
        .network_bandwidth(BURSTABLE_XEN_PEAK_GBPS, BURSTABLE_XEN_PEAK_GBPS)
        .interfaces(3)
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 3.3)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("t2.2xlarge")
        .threads_per_core(1)
        .shared_ebs_attachments(40)
        .network("Moderate")
        .network_bandwidth(BURSTABLE_XEN_PEAK_GBPS, BURSTABLE_XEN_PEAK_GBPS)
        .interfaces(3)
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 3.3)
        .host_features(true, false),
];

/// The Gigabit the Xen burstable family's network device carries, which is the figure every
/// size of it can reach — 1,024 Megabit, the figure of the paravirtual interface its guest is
/// given rather than of an Elastic Network Adapter it has none of.
///
/// The figure a size SUSTAINS is its own step of the family's published baseline ladder, which
/// doubles at each step from 32 Megabit at the smallest size and reaches the device's own
/// figure at the two largest, where the column stops reading `up to` anything.
const BURSTABLE_XEN_PEAK_GBPS: f64 = 1.024;

/// The Gigabit the first burstable family's one size sustains: 70 Megabit, the figure its
/// `Very Low` column stands for.
const T1_BASELINE_GBPS: f64 = 0.07;

/// The Gigabit that same size can reach: 280 Megabit, four times what it sustains.
const T1_PEAK_GBPS: f64 = 0.28;

/// The `RootDeviceType` values a type whose AMIs may be backed by an EBS volume ALONE
/// publishes — what a previous generation family that shipped no local devices reports.
const EBS_ROOT_DEVICE: &[&str] = &["ebs"];
