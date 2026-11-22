// SPDX-License-Identifier: CC0-1.0

//! What `DescribeInstanceTypes` reports for the instance types whose published hardware the
//! catalog now carries row by row — the Neuron accelerated family, the compute optimized
//! family on the newest AWS Graviton hardware and the eighth generation Intel network
//! optimized family with its own EBS hardware — driven over the wire the service speaks
//! (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML; README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: EVERY MEMBER OF A REPORTED INSTANCE TYPE IS THE FIGURE AWS PUBLISHES
//! FOR THAT TYPE, AND A DEVICE INVENTORY AWS REPORTS FOR IT IS REPORTED IN FULL. A type's EBS
//! performance, attachment limit and limit kind, memory, accelerator and Neuron inventories,
//! connection-tracking timeouts and capability flags are per-type facts: they are not a
//! function of the size multiplier in the name, and the family a size belongs to decides them
//! as much as its width does.
//!
//! The rows below span what the rule generalises over rather than one family: an accelerated
//! family whose device count, memory ratio and EBS ladder are all its own, a family whose EBS
//! bandwidth is a baseline at its narrow sizes and dedicated at its wide ones, and a bare
//! metal size that publishes the HOST's features where its virtualized siblings publish the
//! hypervisor's. Each is a row among many, not the specification.

use super::ec2_query::{
    MEGABITS_PER_MEGABYTE, assert_published_ebs, described_instance_type, owned,
    reported_instance_type_list, reported_instance_type_member,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-west-2";

/// The body `DescribeInstanceTypes` answers for exactly one instance type.
async fn described(case: &str, router: &MultiServiceRouter, instance_type: &str) -> String {
    described_instance_type(case, router, REGION, instance_type).await
}

/// The text of the member at `path` inside the one reported instance type, or `None` when the
/// type reports no such member.
fn reported(case: &str, body: &str, path: &[&str]) -> Option<String> {
    reported_instance_type_member(case, body, path)
}

/// The member at `path`, or a panic naming the case when the type reports none: an absent
/// member is exactly what a client reading it cannot act on.
fn required(case: &str, body: &str, path: &[&str]) -> String {
    reported(case, body, path)
        .unwrap_or_else(|| panic!("case {case:?}: no <{}> reported, got {body}", path.join("/")))
}

/// The member at `path` as a whole number.
fn whole(case: &str, body: &str, path: &[&str]) -> i64 {
    let text = required(case, body, path);
    text.parse::<f64>()
        .unwrap_or_else(|_| {
            panic!("case {case:?}: <{}> must be a number, got {text:?}", path.join("/"))
        })
        .round() as i64
}

/// The accelerator inventory AWS publishes for one instance type: how many devices, the
/// NeuronCores each carries and which version those are, and the memory of ONE device in MiB.
type Accelerators = (i64, i64, i64, i64);

/// The EBS-optimized performance AWS publishes for one instance type: the bandwidth and IOPS
/// it sustains, then the bandwidth and IOPS it reaches.
type EbsPerformance = (f64, f64, f64, f64);

/// The hardware figures AWS publishes for one instance type.
#[derive(Debug, Clone, Copy)]
struct Published {
    /// `inf1.6xlarge` and the like.
    instance_type: &'static str,
    /// Its memory in MiB, where the catalog publishes a figure of the type's own rather than
    /// the ratio its family root would give.
    memory_mib: Option<i64>,
    /// Its EBS-optimized performance as (sustained Mbps, sustained IOPS, the Mbps it reaches,
    /// the IOPS it reaches).
    ebs: EbsPerformance,
    /// The EBS volumes it can hold attached, where the catalog publishes a figure of its own.
    ebs_attachments: Option<i64>,
    /// Whether that limit is the volumes' own or shared with its interfaces and devices.
    attachment_limit_type: &'static str,
    /// The idle timeout, in seconds, its connection tracking applies to an established TCP
    /// connection.
    tcp_established_timeout: i64,
    /// Its accelerator devices, or `None` for a type that carries none.
    accelerators: Option<Accelerators>,
}

/// The table every figure test below reads: three sizes of the network optimized family (two
/// that burst above a baseline and the one whose bandwidth is dedicated), four of the newest
/// Graviton family (its narrowest size, the recorded one, the one that holds a whole host and
/// that host without a hypervisor) and three of the Neuron accelerated one.
const PUBLISHED: [Published; 10] = [
    Published {
        instance_type: "c8ine.large",
        memory_mib: None,
        ebs: (650.0, 3_600.0, 10_000.0, 40_000.0),
        ebs_attachments: Some(32),
        attachment_limit_type: "dedicated",
        tcp_established_timeout: 350,
        accelerators: None,
    },
    Published {
        instance_type: "c8ine.2xlarge",
        memory_mib: None,
        ebs: (2_500.0, 12_000.0, 10_000.0, 40_000.0),
        ebs_attachments: Some(32),
        attachment_limit_type: "dedicated",
        tcp_established_timeout: 350,
        accelerators: None,
    },
    Published {
        instance_type: "c8ine.12xlarge",
        memory_mib: None,
        ebs: (15_000.0, 60_000.0, 15_000.0, 60_000.0),
        ebs_attachments: Some(32),
        attachment_limit_type: "dedicated",
        tcp_established_timeout: 350,
        accelerators: None,
    },
    Published {
        instance_type: "c9g.medium",
        memory_mib: None,
        ebs: (380.0, 1_520.0, 12_000.0, 48_000.0),
        ebs_attachments: None,
        attachment_limit_type: "dedicated",
        tcp_established_timeout: 350,
        accelerators: None,
    },
    Published {
        instance_type: "c9g.large",
        memory_mib: None,
        ebs: (760.0, 3_040.0, 12_000.0, 48_000.0),
        ebs_attachments: None,
        attachment_limit_type: "dedicated",
        tcp_established_timeout: 350,
        accelerators: None,
    },
    Published {
        instance_type: "c9g.48xlarge",
        memory_mib: None,
        ebs: (72_000.0, 288_000.0, 72_000.0, 288_000.0),
        ebs_attachments: None,
        attachment_limit_type: "dedicated",
        tcp_established_timeout: 350,
        accelerators: None,
    },
    Published {
        instance_type: "c9g.metal-48xl",
        memory_mib: None,
        ebs: (72_000.0, 288_000.0, 72_000.0, 288_000.0),
        ebs_attachments: None,
        // The bare metal size shares its host's attachment pool with every other device on it,
        // rather than holding EBS hardware of the instance type's own.
        attachment_limit_type: "shared",
        tcp_established_timeout: 350,
        accelerators: None,
    },
    Published {
        instance_type: "inf1.xlarge",
        memory_mib: Some(8 * 1_024),
        ebs: (1_190.0, 6_000.0, 4_750.0, 20_000.0),
        ebs_attachments: Some(26),
        attachment_limit_type: "shared",
        tcp_established_timeout: 432_000,
        accelerators: Some((1, 4, 1, 8_192)),
    },
    Published {
        instance_type: "inf1.6xlarge",
        memory_mib: Some(48 * 1_024),
        ebs: (4_750.0, 20_000.0, 4_750.0, 20_000.0),
        ebs_attachments: Some(26),
        attachment_limit_type: "shared",
        tcp_established_timeout: 432_000,
        accelerators: Some((4, 4, 1, 8_192)),
    },
    Published {
        instance_type: "inf1.24xlarge",
        memory_mib: Some(192 * 1_024),
        ebs: (19_000.0, 80_000.0, 19_000.0, 80_000.0),
        ebs_attachments: Some(26),
        attachment_limit_type: "shared",
        tcp_established_timeout: 432_000,
        accelerators: Some((16, 4, 1, 8_192)),
    },
];

/// Rule: the EBS, memory, attachment and connection-tracking figures a type reports are the
/// ones AWS publishes for THAT type.
#[tokio::test]
async fn published_figures_are_reported_for_every_recorded_family() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in PUBLISHED {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;

        assert_published_ebs(case, &body, row.ebs);

        if let Some(expected) = row.memory_mib {
            let got = whole(case, &body, &["memoryInfo", "sizeInMiB"]);
            assert_eq!(
                got, expected,
                "case {case:?}: expected memoryInfo.sizeInMiB {expected}, got {got}"
            );
        }

        if let Some(expected) = row.ebs_attachments {
            let got = whole(case, &body, &["ebsInfo", "maximumEbsAttachments"]);
            assert_eq!(
                got, expected,
                "case {case:?}: expected maximumEbsAttachments {expected}, got {got}"
            );
        }

        let got = required(case, &body, &["ebsInfo", "attachmentLimitType"]);
        assert_eq!(
            got, row.attachment_limit_type,
            "case {case:?}: expected attachmentLimitType {:?}, got {got:?}",
            row.attachment_limit_type
        );

        let got = whole(
            case,
            &body,
            &[
                "networkInfo",
                "connectionTrackingConfiguration",
                "defaultTcpEstablishedTimeout",
            ],
        );
        assert_eq!(
            got, row.tcp_established_timeout,
            "case {case:?}: expected defaultTcpEstablishedTimeout {}, got {got}",
            row.tcp_established_timeout
        );
    }
}

/// Rule, for the devices: a type that carries accelerators reports BOTH inventories AWS
/// publishes for them — the Inference accelerators and the Neuron devices, the latter naming
/// the NeuronCores one device carries and their version — and a type that carries none reports
/// neither.
#[tokio::test]
async fn accelerator_inventories_are_reported_in_full() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in PUBLISHED {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;

        let Some((count, cores_each, core_version, memory_mib_each)) = row.accelerators else {
            for member in ["inferenceAcceleratorInfo", "neuronInfo"] {
                let got = reported(case, &body, &[member]);
                assert!(
                    got.is_none(),
                    "case {case:?}: a type with no accelerators reports no <{member}>, got \
                     {got:?}"
                );
            }
            continue;
        };

        // The Inference accelerator inventory and the Neuron one describe the SAME devices,
        // the Neuron one naming their cores as well, and each reports the memory of all of
        // them together.
        let figures: [(Vec<&str>, i64); 8] = [
            (
                vec!["inferenceAcceleratorInfo", "accelerators", "member", "count"],
                count,
            ),
            (
                vec![
                    "inferenceAcceleratorInfo",
                    "accelerators",
                    "member",
                    "memoryInfo",
                    "sizeInMiB",
                ],
                memory_mib_each,
            ),
            (
                vec!["inferenceAcceleratorInfo", "totalInferenceMemoryInMiB"],
                count * memory_mib_each,
            ),
            (vec!["neuronInfo", "neuronDevices", "item", "count"], count),
            (
                vec!["neuronInfo", "neuronDevices", "item", "coreInfo", "count"],
                cores_each,
            ),
            (
                vec!["neuronInfo", "neuronDevices", "item", "coreInfo", "version"],
                core_version,
            ),
            (
                vec![
                    "neuronInfo",
                    "neuronDevices",
                    "item",
                    "memoryInfo",
                    "sizeInMiB",
                ],
                memory_mib_each,
            ),
            (
                vec!["neuronInfo", "totalNeuronDeviceMemoryInMiB"],
                count * memory_mib_each,
            ),
        ];
        for (path, expected) in figures {
            let got = whole(case, &body, &path);
            assert_eq!(
                got,
                expected,
                "case {case:?}: expected {} {expected}, got {got}",
                path.join("/")
            );
        }

        // One device description names the device and its manufacturer, under the inventory
        // that publishes each.
        for (path, expected) in [
            (
                vec!["inferenceAcceleratorInfo", "accelerators", "member", "name"],
                "Inferentia",
            ),
            (
                vec![
                    "inferenceAcceleratorInfo",
                    "accelerators",
                    "member",
                    "manufacturer",
                ],
                "AWS",
            ),
            (
                vec!["neuronInfo", "neuronDevices", "item", "name"],
                "Inferentia",
            ),
        ] {
            let got = required(case, &body, &path);
            assert_eq!(
                got,
                expected,
                "case {case:?}: expected {} {expected:?}, got {got:?}",
                path.join("/")
            );
        }
    }
}

/// Rule, for the per-type answers: a capability flag is the published answer for the TYPE,
/// which the bare metal size of a family does not share with its virtualized siblings, and a
/// list a type publishes is its own — reported with its answer whatever that answer is.
#[tokio::test]
async fn published_capability_answers_are_the_types_own() {
    let (router, _engine) = super::ec2_query::emulator();

    let rows: [(&str, Vec<&str>, &str); 19] = [
        ("c8ine.12xlarge", vec!["dedicatedHostsSupported"], "false"),
        ("c8ine.12xlarge", vec!["hibernationSupported"], "false"),
        ("c8ine.12xlarge", vec!["cpuBurstModel"], "none"),
        ("c8ine.large", vec!["phcSupport"], "unsupported"),
        ("c8ine.large", vec!["rebootMigrationSupport"], "supported"),
        (
            "c8ine.large",
            vec!["processorInfo", "sustainedClockSpeedInGhz"],
            "3.9",
        ),
        (
            "c8ine.4xlarge",
            vec!["networkInfo", "enaSrdSupported"],
            "false",
        ),
        (
            "c8ine.4xlarge",
            vec!["networkInfo", "secondaryNetworkSupported"],
            "false",
        ),
        (
            "c8ine.4xlarge",
            vec!["networkInfo", "ipv4AddressesPerSecondaryInterface"],
            "0",
        ),
        (
            "c8ine.4xlarge",
            vec!["networkInfo", "trafficMirroringSupport"],
            "unsupported",
        ),
        // The bare metal size of the newest Graviton family publishes the HOST's features: the
        // clock on its Nitro card, its trusted platform module, the transport its adapter
        // carries, the recovery and the move to another host, none of which a hypervisor it
        // has none of provides.
        ("c9g.metal-48xl", vec!["phcSupport"], "supported"),
        (
            "c9g.metal-48xl",
            vec!["rebootMigrationSupport"],
            "supported",
        ),
        ("c9g.metal-48xl", vec!["nitroTpmSupport"], "supported"),
        ("c9g.metal-48xl", vec!["autoRecoverySupported"], "true"),
        (
            "c9g.metal-48xl",
            vec!["networkInfo", "enaSrdSupported"],
            "true",
        ),
        (
            "c9g.48xlarge",
            vec!["networkInfo", "efaInfo", "maximumEfaInterfaces"],
            "16",
        ),
        // The Neuron accelerated family's adapter encrypts traffic between instances, while
        // its hardware generation does not move a running instance on a reboot.
        (
            "inf1.2xlarge",
            vec!["networkInfo", "encryptionInTransitSupported"],
            "true",
        ),
        ("inf1.2xlarge", vec!["rebootMigrationSupport"], "unsupported"),
        ("inf1.xlarge", vec!["dedicatedHostsSupported"], "false"),
    ];

    for (instance_type, path, expected) in rows {
        let case = format!("{instance_type} {}", path.join("/"));
        let body = described(&case, &router, instance_type).await;
        let got = required(&case, &body, &path);
        assert_eq!(
            got, expected,
            "case {case:?}: expected {} {expected:?}, got {got:?}",
            path.join("/")
        );
    }

    // A family whose network and EBS bandwidth can be weighted publishes the weightings a
    // launch may ask for; one whose split is fixed publishes none at all.
    for instance_type in ["c9g.large", "c9g.metal-48xl"] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let got = reported_instance_type_list(case, &body, &["networkInfo", "bandwidthWeightings"]);
        assert_eq!(
            got,
            owned(&["default", "ebs-1", "vpc-1"]),
            "case {case:?}: expected the published bandwidth weightings, got {got:?}"
        );
    }
    let case = "c8ine.12xlarge";
    let body = described(case, &router, case).await;
    let got = reported(case, &body, &["networkInfo", "bandwidthWeightings"]);
    assert!(
        got.is_none(),
        "case {case:?}: a type whose bandwidth split is fixed publishes no weightings, got \
         {got:?}"
    );
}

/// Rule, stated as what a formula over the name cannot produce: two families of ONE size that
/// publish different EBS figures, a device count that does not follow the width, and one
/// family's two kinds of row — a size that bursts above its baseline and one whose bandwidth
/// is dedicated.
#[tokio::test]
async fn the_published_figures_do_not_follow_the_size_in_the_name() {
    let (router, _engine) = super::ec2_query::emulator();

    let baseline = |case: &str, body: &str| -> i64 {
        whole(case, body, &["ebsInfo", "ebsOptimizedInfo", "baselineBandwidthInMbps"])
    };
    let maximum = |case: &str, body: &str| -> i64 {
        whole(case, body, &["ebsInfo", "ebsOptimizedInfo", "maximumBandwidthInMbps"])
    };

    // One size component, two families: each publishes its own sustained bandwidth, so neither
    // can be read off the `12xlarge`.
    let intel = described("c8ine.12xlarge", &router, "c8ine.12xlarge").await;
    let graviton = described("c9g.12xlarge", &router, "c9g.12xlarge").await;
    let intel_baseline = baseline("c8ine.12xlarge", &intel);
    let graviton_baseline = baseline("c9g.12xlarge", &graviton);
    assert_ne!(
        intel_baseline, graviton_baseline,
        "two families of one size publish their own EBS bandwidth, got {intel_baseline} for \
         both"
    );

    // One family, two sizes: the wider one carries four times the devices of the narrower
    // while its vCPUs only treble, so the inventory is not a multiple of the width.
    let narrow = described("inf1.2xlarge", &router, "inf1.2xlarge").await;
    let wide = described("inf1.6xlarge", &router, "inf1.6xlarge").await;
    let devices = |case: &str, body: &str| -> i64 {
        whole(case, body, &["neuronInfo", "neuronDevices", "item", "count"])
    };
    let vcpus = |case: &str, body: &str| -> i64 { whole(case, body, &["vCpuInfo", "defaultVCpus"]) };
    let (narrow_devices, wide_devices) = (devices("inf1.2xlarge", &narrow), devices("inf1.6xlarge", &wide));
    let (narrow_vcpus, wide_vcpus) = (vcpus("inf1.2xlarge", &narrow), vcpus("inf1.6xlarge", &wide));
    assert_eq!(
        wide_devices,
        narrow_devices * 4,
        "the wider size carries four times the devices, got {narrow_devices} and {wide_devices}"
    );
    assert_eq!(
        wide_vcpus,
        narrow_vcpus * 3,
        "while its vCPUs only treble, got {narrow_vcpus} and {wide_vcpus}"
    );

    // One family, its two kinds of published row: a size below the dedicated one sustains its
    // share and bursts above it, and the dedicated size sustains what it reaches.
    let bursting = described("c8ine.4xlarge", &router, "c8ine.4xlarge").await;
    let dedicated = described("c8ine.8xlarge", &router, "c8ine.8xlarge").await;
    let bursting_baseline = baseline("c8ine.4xlarge", &bursting);
    let bursting_maximum = maximum("c8ine.4xlarge", &bursting);
    assert!(
        bursting_maximum > bursting_baseline,
        "a size below the dedicated row bursts above its baseline, got \
         {bursting_baseline}/{bursting_maximum}"
    );
    let dedicated_baseline = baseline("c8ine.8xlarge", &dedicated);
    let dedicated_maximum = maximum("c8ine.8xlarge", &dedicated);
    assert_eq!(
        dedicated_baseline, dedicated_maximum,
        "the dedicated size sustains what it reaches, got \
         {dedicated_baseline}/{dedicated_maximum}"
    );

    // And the two units of one published bandwidth agree: the MB/s member is the Mbps one in
    // the other unit, which is how the published table's two columns relate.
    let throughput = {
        let case = "c9g.large";
        let body = described(case, &router, case).await;
        let text = required(
            case,
            &body,
            &["ebsInfo", "ebsOptimizedInfo", "baselineThroughputInMBps"],
        );
        text.parse::<f64>()
            .unwrap_or_else(|_| panic!("case {case:?}: expected a number, got {text:?}"))
    };
    let expected = 760.0 / MEGABITS_PER_MEGABYTE;
    assert!(
        (throughput - expected).abs() < 0.001,
        "the sustained bandwidth in MB/s is the Mbps figure in the other unit, expected \
         {expected}, got {throughput}"
    );
}
