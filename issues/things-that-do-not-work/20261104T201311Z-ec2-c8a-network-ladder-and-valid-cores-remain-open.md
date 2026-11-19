Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-catalog-describe-instance-types-a1-c9g
Issue-Step: task#23
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-catalog-describe-instance-types-a1-c9g
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.medium"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.medium"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.medium"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.medium"].networkInfo.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.large"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.large"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.large"].networkInfo.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.xlarge"].networkInfo.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.2xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.2xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.2xlarge"].networkInfo.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.4xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.4xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.4xlarge"].networkInfo.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.8xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.8xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.8xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.8xlarge"].networkInfo.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.12xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.12xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.12xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.12xlarge"].networkInfo.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.12xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.16xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.16xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.16xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.16xlarge"].networkInfo.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.16xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.24xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.24xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.24xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.24xlarge"].networkInfo.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.24xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.48xlarge"].ebsInfo.ebsOptimizedInfo.baselineBandwidthInMbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.48xlarge"].ebsInfo.ebsOptimizedInfo.baselineThroughputInMBps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.48xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.48xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.48xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.48xlarge"].networkInfo.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.48xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.metal-24xl"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.metal-24xl"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.metal-24xl"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.metal-24xl"].networkInfo.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.metal-48xl"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.metal-48xl"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.metal-48xl"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8a.metal-48xl"].networkInfo.networkPerformance
Issue-Commit: 26511efcb51aa0c8f5bf3ace614d1ff380fd4a98

# ec2 DescribeInstanceTypes: the c8a family's network-interface ladder, its network performance, its wide sizes' valid core counts and its largest size's sustained EBS bandwidth are still the size-ladder derivation

> _Split out of `issues/things-that-now-work/20261104T201311Z-ec2-instance-type-catalog-is-synthesized-per-family.md`, whose five causes are otherwise fixed: that fix added per-type published rows for the c8a, gr6, m3, r5a and t3a families. This report carries the replay keys that do NOT pass, re-filed here so they are not recorded as working. Everything below is what that fix established about them; a later fixer starts from here rather than from scratch._

## Operation
DescribeInstanceTypes

## Description
The parent fix put the c8a family's published EBS performance, attachment limit and limit
type, bandwidth weightings, connection-tracking timeout, ENA Express support,
secondary-network members, processor frequency and vCPU/core/thread arrangement into
`services/service-com-amazonaws-ec2-amazonec2/src/state/instance_ids_and_instance_types/instance_type_catalog/amd_epyc_rows.rs`,
one row per instance type. FOUR groups of members the same rule covers are still answered by
the size-ladder derivation in `state/instance_ids_and_instance_types.rs`, because no published
figure for them was available to that fix and a wire replay withholds the recorded values:

Cause 1 of 4: `networkInfo.maximumNetworkInterfaces` and
`networkInfo.ipv4AddressesPerInterface` / `ipv6AddressesPerInterface`, for every c8a size.
AWS publishes these per instance type ("IP addresses per network interface per instance
type"), and the figures it publishes for this family are NOT the ones every other recorded
family of the same hardware generation carries: the Graviton family of the same size ladder
(`c8g`) matches the derivation's rows at those sizes — its recorded page reports 4 interfaces
of 15 addresses at the `2xlarge`, 8 of 30 at the `12xlarge` and 15 of 50 from the `16xlarge`
up — while the c8a page diverges from all of them. The same divergence is open on the 8th
generation Intel family `r8ib` (see
`issues/things-that-now-work/20261031T012056Z-ec2-describeinstancetypes-omits-modeled-per-type-members-and.md`,
whose rows also leave these members derived), so what is missing is the published
interface/address ladder of the newest generation rather than one family's row.
Emulator: `network_limits` in `state/instance_ids_and_instance_types.rs` reads the ladder's
step — (3, 10) at `large`, (4, 15) through the `3xlarge`, (8, 30) through the `12xlarge`,
(15, 50) from the `16xlarge` up — and `ipv6AddressesPerInterface` repeats the IPv4 figure, so
both members follow the size multiplier rather than the type.
Fix: the `interfaces` and `ipv4_per_interface` columns of `PublishedInstanceTypeFacts` already
hold these per type (the `c8g`, `r5b` and `t3` rows use them); the c8a rows need the published
figures, which a recorded `DescribeInstanceTypes` page for the family would supply.
Traces: ec2-catalog-describe-instance-types-a1-c9g#task#23.

Cause 2 of 4: `networkInfo.networkPerformance`, for every c8a size.
AWS publishes the words of the "Network performance" column per instance type ("Up to 12.5
Gigabit", "22.5 Gigabit"), a fact of the type's share of its host's adapter.
Emulator: `network_performance` derives the string from the size's share of a 25 Gbps host and
the 10 Gbps burst ceiling, so every size below its host's full share reports "Up to 10
Gigabit". Nothing is known about this family's published column, and the figure also feeds the
single `networkInfo.networkCards` entry, so writing a guess there would move a member that
currently matches.
Fix: the `network` column holds the published words per type; the c8a rows need the family's
published column.
Traces: ec2-catalog-describe-instance-types-a1-c9g#task#23.

Cause 3 of 4: `vCpuInfo.validCores` on the sizes above the `8xlarge` (`12xlarge`, `16xlarge`,
`24xlarge`, `48xlarge`).
AWS publishes the core counts a type can be configured with ("Optimize CPU options"). The
parent fix established that this family ships with simultaneous multithreading DISABLED — its
`defaultCores` equals its vCPUs and `validThreadsPerCore` is `[1]`, which the replay now
reports as passing, and the `2xlarge`'s `validCores` passes with every count from one up to its
eight cores — so the published lists of the WIDER sizes are not simply every count up to the
default. The parent fix tried the next hypothesis, the counts going up two at a time from the
`12xlarge` (the step the recorded `r8ib` rows use from their `24xlarge` up), and the replay
showed it is not AWS's list either.
Emulator: `InstanceTypeRecord::valid_cores` lists every count from the row's minimum up to
`defaultCores` in the row's step.
Fix: the `valid_core_step` / `valid_core_minimum` columns hold the shape per type; the c8a rows
need the published lists.
Traces: ec2-catalog-describe-instance-types-a1-c9g#task#23.

Cause 4 of 4: `ebsInfo.ebsOptimizedInfo.baselineBandwidthInMbps` and
`baselineThroughputInMBps` on `c8a.48xlarge` alone.
AWS publishes the sustained and the maximum EBS bandwidth per type ("Amazon EBS optimized
instance types"). Every other c8a size now reports the published pair, and this size's
`maximumBandwidthInMbps`, `maximumThroughputInMBps`, `baselineIops` and `maximumIops` pass with
40,000 Mbps and 240,000 IOPS — so the row's maximum is right and its SUSTAINED bandwidth is
not: the size that holds the whole host publishes a baseline BELOW its maximum, unlike the
sibling Graviton family, whose largest size sustains its maximum.
Emulator: the row at `amd_epyc_rows.rs` publishes 40,000 Mbps as both figures.
Fix: that row's `ebs(...)` baseline needs the family's published sustained figure.
Traces: ec2-catalog-describe-instance-types-a1-c9g#task#23.

## Evidence
Replay of `ec2-catalog-describe-instance-types-a1-c9g#task#23` against the emulator built from
the parent fix: the step is still a 200 -> 200 body mismatch, and the keys that remain are the
ones above — for `c8a.12xlarge`, `16xlarge` and `24xlarge`
`networkInfo.ipv4AddressesPerInterface`, `ipv6AddressesPerInterface`,
`maximumNetworkInterfaces`, `networkPerformance` and `vCpuInfo.validCores.item[*]`; for
`c8a.2xlarge` the three network members alone (its `validCores` and its interface count now
pass); and for `c8a.48xlarge` those plus
`ebsInfo.ebsOptimizedInfo.baselineBandwidthInMbps` / `baselineThroughputInMBps`. Every other
key of the parent report's five causes — the EBS figures and limits, the attachment limit type,
the bandwidth weightings, the connection-tracking timeout, ENA Express, the secondary-network
members, the processor, the memory, the instance store, the accelerators, the host features and
IPv6 — no longer fails.

## Steps to Reproduce
1. Clone the repo and build/boot the emulator from this checkout.
2. Replay trace set simparity-wire-aws-v1, fixture ec2-catalog-describe-instance-types-a1-c9g,
   step task#23 (the c8a family page).
3. Compare each `instanceTypeSet` item field by field against the fixture's recorded response:
   the members listed above are the ones that still differ, and each of them is a figure AWS
   publishes per instance type rather than a function of the size in its name.
