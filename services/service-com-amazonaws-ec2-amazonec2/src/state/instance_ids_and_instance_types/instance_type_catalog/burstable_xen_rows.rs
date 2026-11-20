// SPDX-License-Identifier: CC0-1.0

//! The published rows of the burstable performance family that runs on the Xen hypervisor,
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
//! The figures come from the family's own specification ("Burstable performance instances"
//! and the T2 specification table: the network performance words and the processor's
//! frequency), "Instance volume limits" (the shared attachment total), the published list of
//! the types offered enhanced networking with ENA, which does not name this family, and the
//! "IP addresses per network interface per instance type" table (the interface and address
//! limits of the sizes whose row is not the one their step of the size ladder publishes).

use super::*;

/// The published hardware facts of the burstable Xen family's instance types, one row per
/// type.
pub(super) static BURSTABLE_XEN_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
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
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 3.3)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("t2.micro")
        .threads_per_core(1)
        .shared_ebs_attachments(40)
        .network("Low to Moderate")
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 3.3)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("t2.small")
        .threads_per_core(1)
        .shared_ebs_attachments(40)
        .network("Low to Moderate")
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 3.3)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("t2.medium")
        .threads_per_core(1)
        .shared_ebs_attachments(40)
        .network("Low to Moderate")
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 3.3)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("t2.large")
        .threads_per_core(1)
        .shared_ebs_attachments(40)
        .network("Low to Moderate")
        .ipv4_per_interface(12)
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 3.3)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("t2.xlarge")
        .threads_per_core(1)
        .shared_ebs_attachments(40)
        .network("Moderate")
        .interfaces(3)
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 3.3)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("t2.2xlarge")
        .threads_per_core(1)
        .shared_ebs_attachments(40)
        .network("Moderate")
        .interfaces(3)
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 3.3)
        .host_features(true, false),
];
