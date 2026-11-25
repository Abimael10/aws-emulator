// SPDX-License-Identifier: CC0-1.0

//! What `DescribeInstanceTypes` serves for a general purpose instance type: the hardware facts
//! AWS publishes PER TYPE, rather than figures read off the size multiplier in its name.
//!
//! Every call goes over the wire the service speaks, through the shared `super::ec2_query`
//! harness, and every row below is a type of the frequency-optimized fifth generation family,
//! the sixth generation AMD one or the sixth generation Intel one.
//!
//! The rules under test:
//!
//! 1. A type's EBS performance, its volume limit, its network column and its processor's
//!    sustained frequency are the figures published for THAT type: two sizes of one family do
//!    not scale by the size multiplier, and a size that holds a whole host reports its host's
//!    own attachment pool.
//! 2. The features a type is offered — an Elastic Fabric Adapter, ENA Express, the core counts
//!    it can be configured with, the interfaces it can attach and the addresses each of them
//!    takes — are facts of the SIZE, so one family reports different answers across its ladder.
//! 3. Whether IPv6 reaches a type is a published fact of its own and not the generation flag:
//!    a family AWS reports as no longer the latest generation of its family still answers with
//!    IPv6 addresses per interface, while an older family AWS offers no IPv6 on answers none at
//!    all.

use super::ec2_query::{
    self, assert_published_ebs, described_instance_type, reported_instance_type_list,
    reported_instance_type_member,
};

/// The Region the catalog is read for.
const REGION: &str = "us-west-2";

/// The member at `path` of the one reported type, or a panic naming the case when the type
/// reports no such member.
fn member(case: &str, body: &str, path: &[&str]) -> String {
    reported_instance_type_member(case, body, path).unwrap_or_else(|| {
        panic!("case {case:?}: the type reports no {path:?} at all, got {body}")
    })
}

/// What one published row serves: the four EBS figures of its table row, the volumes it holds
/// attached, the network column as the published table writes it and its processor's sustained
/// frequency.
struct HardwareRow {
    case: &'static str,
    instance_type: &'static str,
    /// `(sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches)`.
    ebs: (f64, f64, f64, f64),
    /// `EbsInfo$MaximumEbsAttachments`.
    attachments: &'static str,
    /// `NetworkInfo$NetworkPerformance`.
    network: &'static str,
    /// `ProcessorInfo$SustainedClockSpeedInGhz`.
    clock_ghz: &'static str,
}

/// Rule 1: each row's figures are the ones published for that instance type.
///
/// The rows differ along what the rule generalises over: the family (a fifth generation
/// frequency-optimized one, a sixth generation AMD one and a sixth generation Intel one), the
/// place of the size on its ladder (a size that bursts above its sustained bandwidth, the size
/// whose bandwidth is first dedicated, a size whose IOPS keep growing after its bandwidth has
/// reached the host's figure, the largest virtualized size and the bare metal one) and the
/// processor's designer. The figures are the per-type rows of the "Amazon EBS optimized
/// instance types" table, the published network column of each family's specification page and
/// the attachments "Instance volume limits" publishes, read here through the catalog
/// `crate::state::instance_type_info` projects.
#[tokio::test]
async fn a_published_row_serves_the_type_s_own_hardware_figures() {
    let (router, _engine) = ec2_query::emulator();
    let rows: [HardwareRow; 10] = [
        HardwareRow {
            case: "a size that bursts above its sustained bandwidth",
            instance_type: "m5zn.large",
            ebs: (800.0, 3_333.0, 3_170.0, 13_333.0),
            attachments: "27",
            network: "Up to 25 Gigabit",
            clock_ghz: "4.5",
        },
        HardwareRow {
            case: "a size whose bandwidth is dedicated",
            instance_type: "m5zn.3xlarge",
            ebs: (4_750.0, 20_000.0, 4_750.0, 20_000.0),
            attachments: "27",
            network: "Up to 25 Gigabit",
            clock_ghz: "4.5",
        },
        HardwareRow {
            case: "the largest virtualized size of a frequency-optimized family",
            instance_type: "m5zn.12xlarge",
            ebs: (19_000.0, 80_000.0, 19_000.0, 80_000.0),
            attachments: "27",
            network: "100 Gigabit",
            clock_ghz: "4.5",
        },
        HardwareRow {
            case: "a bare metal size, which holds its host's own attachment pool",
            instance_type: "m5zn.metal",
            ebs: (19_000.0, 80_000.0, 19_000.0, 80_000.0),
            attachments: "31",
            network: "100 Gigabit",
            clock_ghz: "4.5",
        },
        HardwareRow {
            case: "the narrowest size of a sixth generation AMD family",
            instance_type: "m6a.large",
            ebs: (650.0, 3_600.0, 10_000.0, 40_000.0),
            attachments: "27",
            network: "Up to 12.5 Gigabit",
            clock_ghz: "3.6",
        },
        HardwareRow {
            case: "the AMD size whose bandwidth is first dedicated",
            instance_type: "m6a.8xlarge",
            ebs: (10_000.0, 40_000.0, 10_000.0, 40_000.0),
            attachments: "27",
            network: "12.5 Gigabit",
            clock_ghz: "3.6",
        },
        HardwareRow {
            case: "an AMD size a third of the way up the ladder",
            instance_type: "m6a.12xlarge",
            ebs: (15_000.0, 60_000.0, 15_000.0, 60_000.0),
            attachments: "27",
            network: "18.75 Gigabit",
            clock_ghz: "3.6",
        },
        HardwareRow {
            case: "an AMD size whose bandwidth has reached its host's figure",
            instance_type: "m6a.48xlarge",
            ebs: (40_000.0, 240_000.0, 40_000.0, 240_000.0),
            attachments: "27",
            network: "50 Gigabit",
            clock_ghz: "3.6",
        },
        HardwareRow {
            case: "the AMD bare metal size",
            instance_type: "m6a.metal",
            ebs: (40_000.0, 240_000.0, 40_000.0, 240_000.0),
            attachments: "31",
            network: "50 Gigabit",
            clock_ghz: "3.6",
        },
        HardwareRow {
            case: "a sixth generation Intel size",
            instance_type: "m6i.2xlarge",
            ebs: (2_500.0, 12_000.0, 10_000.0, 40_000.0),
            attachments: "27",
            network: "Up to 12.5 Gigabit",
            clock_ghz: "3.5",
        },
    ];

    for row in rows {
        let case = row.case;
        let body = described_instance_type(case, &router, REGION, row.instance_type).await;
        assert_published_ebs(case, &body, row.ebs);

        let attachments = member(case, &body, &["ebsInfo", "maximumEbsAttachments"]);
        assert_eq!(
            attachments, row.attachments,
            "case {case:?}: expected maximumEbsAttachments {} for {}, got {attachments:?}",
            row.attachments, row.instance_type
        );

        let network = member(case, &body, &["networkInfo", "networkPerformance"]);
        assert_eq!(
            network, row.network,
            "case {case:?}: expected networkPerformance {:?} for {}, got {network:?}",
            row.network, row.instance_type
        );

        let clock = member(case, &body, &["processorInfo", "sustainedClockSpeedInGhz"]);
        assert_eq!(
            clock, row.clock_ghz,
            "case {case:?}: expected sustainedClockSpeedInGhz {} for {}, got {clock:?}",
            row.clock_ghz, row.instance_type
        );
    }
}

/// Rule 1, the invariant behind the rows: a published row is a FACT of the type, so the
/// figures of two sizes of one family do not stand in the ratio of their sizes.
///
/// Doubling the size does not double the sustained bandwidth, and the size that holds the
/// whole host reaches the same bandwidth as the one two thirds its size while its IOPS keep
/// growing — neither of which any function of the size multiplier yields.
#[tokio::test]
async fn one_family_s_sizes_do_not_scale_by_their_size_multiplier() {
    let (router, _engine) = ec2_query::emulator();
    let ebs_figure = |case: &str, body: &str, member_name: &str| -> f64 {
        let text = member(
            case,
            body,
            &["ebsInfo", "ebsOptimizedInfo", member_name],
        );
        text.parse().unwrap_or_else(|_| {
            panic!("case {case:?}: <{member_name}> must be a number, got {text:?}")
        })
    };

    let case = "twice the size does not sustain twice the bandwidth";
    let large = described_instance_type(case, &router, REGION, "m5zn.large").await;
    let xlarge = described_instance_type(case, &router, REGION, "m5zn.2xlarge").await;
    let sustained_large = ebs_figure(case, &large, "baselineBandwidthInMbps");
    let sustained_2xlarge = ebs_figure(case, &xlarge, "baselineBandwidthInMbps");
    assert!(
        (sustained_2xlarge - 2.0 * sustained_large).abs() > 1.0,
        "case {case:?}: m5zn.2xlarge sustains {sustained_2xlarge} Mbps, which is twice the \
         {sustained_large} Mbps of m5zn.large — a size-derived figure rather than a published one"
    );
    assert!(
        sustained_2xlarge > sustained_large,
        "case {case:?}: the wider size must sustain more, got {sustained_2xlarge} against \
         {sustained_large}"
    );

    let case = "a size wider than the one that reaches the host's bandwidth";
    let thirty_two = described_instance_type(case, &router, REGION, "m6a.32xlarge").await;
    let forty_eight = described_instance_type(case, &router, REGION, "m6a.48xlarge").await;
    for member_name in ["baselineBandwidthInMbps", "maximumBandwidthInMbps"] {
        let narrower = ebs_figure(case, &thirty_two, member_name);
        let wider = ebs_figure(case, &forty_eight, member_name);
        assert!(
            (wider - narrower).abs() < 1.0,
            "case {case:?}: both sizes must report the host's own <{member_name}>, got {wider} \
             for m6a.48xlarge against {narrower} for m6a.32xlarge"
        );
    }
    let narrower_iops = ebs_figure(case, &thirty_two, "baselineIops");
    let wider_iops = ebs_figure(case, &forty_eight, "baselineIops");
    assert!(
        wider_iops > narrower_iops,
        "case {case:?}: the wider size's published IOPS must still be the greater, got \
         {wider_iops} against {narrower_iops}"
    );
}

/// What one row publishes about the features of its SIZE.
struct FeatureRow {
    case: &'static str,
    instance_type: &'static str,
    /// `NetworkInfo$EfaSupported`.
    efa: &'static str,
    /// `NetworkInfo$EnaSrdSupported`.
    ena_srd: &'static str,
    /// `NetworkInfo$MaximumNetworkInterfaces` and `$Ipv4AddressesPerInterface`.
    interfaces: &'static str,
    addresses: &'static str,
    /// `VCpuInfo$ValidCores`, as the published list of the counts the size can be configured
    /// with.
    valid_cores: &'static [&'static str],
}

/// Rule 2: the features a size is offered are published per size, so one family answers
/// differently across its ladder.
///
/// The rows differ along the size's place on the ladder (one below the size an Elastic Fabric
/// Adapter is first offered at, the two sizes that hold a whole host, the size ENA Express is
/// first offered at and the one below it), and along what each feature generalises over: the
/// adapter the size carries, the interfaces and addresses of its row of the published "IP
/// addresses per network interface per instance type" table, and the core counts it can be
/// configured with — every count, the even ones from a quarter of them, or its default alone.
#[tokio::test]
async fn a_size_reports_the_features_its_own_row_publishes() {
    let (router, _engine) = ec2_query::emulator();
    let rows: [FeatureRow; 6] = [
        FeatureRow {
            case: "the size below the one an Elastic Fabric Adapter is offered at",
            instance_type: "m5zn.6xlarge",
            efa: "false",
            ena_srd: "false",
            interfaces: "8",
            addresses: "30",
            valid_cores: &["2", "4", "6", "8", "10", "12"],
        },
        FeatureRow {
            case: "a size that holds its family's whole network share",
            instance_type: "m5zn.12xlarge",
            efa: "true",
            ena_srd: "false",
            interfaces: "15",
            addresses: "50",
            valid_cores: &["24"],
        },
        FeatureRow {
            case: "a size whose row publishes its own interface limits",
            instance_type: "m5zn.3xlarge",
            efa: "false",
            ena_srd: "false",
            interfaces: "8",
            addresses: "30",
            valid_cores: &["2", "4", "6"],
        },
        FeatureRow {
            case: "the AMD size below the one ENA Express is offered at",
            instance_type: "m6a.8xlarge",
            efa: "false",
            ena_srd: "false",
            interfaces: "8",
            addresses: "30",
            valid_cores: &["4", "6", "8", "10", "12", "14", "16"],
        },
        FeatureRow {
            case: "the AMD size ENA Express is first offered at",
            instance_type: "m6a.12xlarge",
            efa: "false",
            ena_srd: "true",
            interfaces: "8",
            addresses: "30",
            valid_cores: &["24"],
        },
        FeatureRow {
            case: "the AMD size that holds the whole host",
            instance_type: "m6a.48xlarge",
            efa: "true",
            ena_srd: "true",
            interfaces: "15",
            addresses: "50",
            valid_cores: &["96"],
        },
    ];

    for row in rows {
        let case = row.case;
        let body = described_instance_type(case, &router, REGION, row.instance_type).await;
        for (path, expected) in [
            (["networkInfo", "efaSupported"], row.efa),
            (["networkInfo", "enaSrdSupported"], row.ena_srd),
            (["networkInfo", "maximumNetworkInterfaces"], row.interfaces),
            (["networkInfo", "ipv4AddressesPerInterface"], row.addresses),
        ] {
            let actual = member(case, &body, &path);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {path:?} {expected:?} for {}, got {actual:?}",
                row.instance_type
            );
        }

        let efa_info = reported_instance_type_member(case, &body, &["networkInfo", "efaInfo"]);
        assert_eq!(
            efa_info.is_some(),
            row.efa == "true",
            "case {case:?}: {} reports efaInfo {efa_info:?} beside efaSupported {:?}",
            row.instance_type,
            row.efa
        );

        let cores = reported_instance_type_list(case, &body, &["vCpuInfo", "validCores"]);
        assert_eq!(
            cores, row.valid_cores,
            "case {case:?}: expected the published core counts {:?} for {}, got {cores:?}",
            row.valid_cores, row.instance_type
        );
    }
}

/// Rule 3: whether IPv6 reaches a type is published per family, not read off the generation
/// flag — so a family AWS no longer reports as the latest generation of its family still
/// answers with IPv6 addresses per interface, and an interface of a type AWS offers no IPv6 on
/// takes none at all.
///
/// The rows differ along the two facts the rule keeps apart: a previous generation family that
/// IS offered IPv6 (three sizes of it, so the answer is the family's rather than one size's), a
/// current generation family, and a previous generation family that is NOT offered it.
#[tokio::test]
async fn ipv6_support_is_published_apart_from_the_generation_flag() {
    let (router, _engine) = ec2_query::emulator();
    let rows: [(&str, &str, &str, &str); 6] = [
        (
            "a previous generation size offered IPv6",
            "m6i.large",
            "false",
            "true",
        ),
        (
            "another size of the same family",
            "m6i.2xlarge",
            "false",
            "true",
        ),
        (
            "the size of it that holds the whole host",
            "m6i.32xlarge",
            "false",
            "true",
        ),
        (
            "a current generation size",
            "m6a.large",
            "true",
            "true",
        ),
        (
            "a current generation frequency-optimized size",
            "m5zn.12xlarge",
            "true",
            "true",
        ),
        (
            "a previous generation family offered no IPv6",
            "m3.medium",
            "false",
            "false",
        ),
    ];

    for (case, instance_type, current_generation, ipv6) in rows {
        let body = described_instance_type(case, &router, REGION, instance_type).await;
        let generation = member(case, &body, &["currentGeneration"]);
        assert_eq!(
            generation, current_generation,
            "case {case:?}: expected currentGeneration {current_generation:?} for \
             {instance_type}, got {generation:?}"
        );
        let supported = member(case, &body, &["networkInfo", "ipv6Supported"]);
        assert_eq!(
            supported, ipv6,
            "case {case:?}: expected ipv6Supported {ipv6:?} for {instance_type}, got \
             {supported:?}"
        );
        let addresses = member(case, &body, &["networkInfo", "ipv6AddressesPerInterface"]);
        let private = member(case, &body, &["networkInfo", "ipv4AddressesPerInterface"]);
        let expected = if ipv6 == "true" { private.clone() } else { "0".to_string() };
        assert_eq!(
            addresses, expected,
            "case {case:?}: an interface of {instance_type} takes {private} private addresses, \
             so it must report {expected} IPv6 ones, got {addresses:?}"
        );
    }
}
