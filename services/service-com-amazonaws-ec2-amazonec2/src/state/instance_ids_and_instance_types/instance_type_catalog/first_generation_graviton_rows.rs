// SPDX-License-Identifier: CC0-1.0

//! The published rows of the FIRST generation AWS Graviton general purpose family, read
//! through [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! The family sits on a 16-vCPU Graviton host whose network adapter is a 10 Gigabit one, and
//! that one figure is what every size of the ladder publishes — the specification's "Network
//! performance" column reads `Up to 10 Gigabit` at the `medium` and still reads it at the
//! `4xlarge` and the bare metal size, which hold the WHOLE host. No share of a later
//! generation's 25 Gigabit host is involved at any step, which is what makes these rows
//! necessary: the figure a size can reach is the adapter's whole 10 Gigabit at EVERY size,
//! and the figure it sustains is its own step of the published baseline ladder — half a
//! Gigabit at the `medium`, and doubling from the `xlarge` up without ever reaching the
//! adapter's own figure, which is why the column reads `Up to` at every size.
//!
//! The figures come from the family's own instance-type specification page (the network
//! performance column and the vCPUs of each size) and the "IP addresses per network interface
//! per instance type" table, whose row for the smallest size publishes fewer interfaces and
//! addresses than that step of the generic size ladder gives.

use super::*;

/// The published hardware facts of the first generation AWS Graviton family's instance types,
/// one row per type.
pub(super) static FIRST_GENERATION_GRAVITON_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // A1 — general purpose on the first AWS Graviton processor, with no local instance store.
    // Every size publishes the same network performance column ([`A1_NETWORK_PERFORMANCE`])
    // and the same ceiling ([`A1_ADAPTER_PEAK_GBPS`]); they differ only in the figure they
    // sustain, which is the ladder's own step rather than a share of a host — and which stays
    // below the adapter's figure even on the two sizes that hold every vCPU of the host, so
    // the column reads `Up to` there too. The smallest size takes TWO interfaces with four
    // private addresses each, where that step of the generic ladder publishes three interfaces
    // and ten addresses.
    PublishedInstanceTypeFacts::of("a1.medium")
        .network(A1_NETWORK_PERFORMANCE)
        .network_bandwidth(0.5, A1_ADAPTER_PEAK_GBPS)
        .interfaces(2)
        .ipv4_per_interface(4),
    PublishedInstanceTypeFacts::of("a1.large")
        .network(A1_NETWORK_PERFORMANCE)
        .network_bandwidth(0.75, A1_ADAPTER_PEAK_GBPS),
    PublishedInstanceTypeFacts::of("a1.xlarge")
        .network(A1_NETWORK_PERFORMANCE)
        .network_bandwidth(1.25, A1_ADAPTER_PEAK_GBPS),
    PublishedInstanceTypeFacts::of("a1.2xlarge")
        .network(A1_NETWORK_PERFORMANCE)
        .network_bandwidth(2.5, A1_ADAPTER_PEAK_GBPS),
    PublishedInstanceTypeFacts::of("a1.4xlarge")
        .network(A1_NETWORK_PERFORMANCE)
        .network_bandwidth(5.0, A1_ADAPTER_PEAK_GBPS),
    PublishedInstanceTypeFacts::of("a1.metal")
        .network(A1_NETWORK_PERFORMANCE)
        .network_bandwidth(5.0, A1_ADAPTER_PEAK_GBPS),
];

/// "The network performance" every size of the family publishes: the specification's column
/// reads the same at every step of the ladder, the whole family sharing one adapter figure.
const A1_NETWORK_PERFORMANCE: &str = "Up to 10 Gigabit";

/// The Gigabit the family's network adapter carries, which is the figure every size can
/// reach.
const A1_ADAPTER_PEAK_GBPS: f64 = 10.0;
