Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-catalog-describe-instance-types-a1-c9g
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-catalog-describe-instance-types-a1-c9g
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5ad.12xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5ad.16xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5ad.24xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5ad.4xlarge"].ebsInfo.ebsOptimizedInfo.baselineIops
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5ad.4xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5ad.8xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5n.9xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Related: 20261113T004910Z-ec2-describe-read-paths-return-stale-and-incomplete.md
Issue-Related: 20261114T052927Z-ec2-static-catalogs-serve-synthesized-records-instance-types.md
Issue-Commit: 0771e25bba8212ca0e4339575fc1faa7cfb6765d

# ec2 DescribeInstanceTypes serves synthesized network card and vCPU facts

> _Note: this issue was identified against the code base at commit `0771e25bba8212ca0e4339575fc1faa7cfb6765d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceTypes

## Description
DescribeInstanceTypes answers 200 but the per-type hardware facts it serves are computed from emulator defaults rather than the values AWS publishes, and several modeled members are never emitted. 150 divergence clusters across the ec2-catalog-describe-instance-types-* fixtures are NOT owned by any open issue and are grounded in this one root cause - the emulator's instance-type catalog table. The similar open issues 20261112T182924Z-ec2-describeinstancetypes-rows-for-the-c4-c9gd-m6i.md and 20261111T180943Z-ec2-read-paths-serve-emulator-derived-catalog-data.md carry other steps of the same fixtures only, so they do not cover the steps below.

GENERAL FAULT
AWS: each InstanceTypeInfo row carries that type's published facts. Smithy: NetworkCardInfo.BaselineBandwidthInGbps / PeakBandwidthInGbps / DefaultEnaQueueCountPerInterface / InterfaceTypes (xmlName interfaceTypeSet), VCpuInfo.ValidCores, EbsOptimizedInfo.BaselineIops, InstanceTypeInfo.FreeTierEligible and RebootMigrationSupport.
Emulator: network-card bandwidths are derived arithmetically (values such as 8.33, 11.11, 5.55 - a ratio computed from the type's size rather than the published figure), defaultEnaQueueCountPerInterface is never emitted, validCores is a dense 1..vCPU enumeration instead of the published set of selectable core counts, interfaceTypeSet omits 'efa-only' on EFA-capable types, and ebsOptimizedInfo.baselineIops and the freeTierEligible / rebootMigrationSupport flags are defaults.
Why: capacity planners, autoscaling sizing logic and CPU-options validation read these fields. A client choosing CpuOptions.CoreCount from validCores offers values AWS rejects (and cannot offer the ones AWS accepts), ENA queue tuning sees no queue count at all, EFA-only placement is impossible, and network sizing is off by whole multiples.
Fix: the instance-type catalog data the DescribeInstanceTypes handler reads (the static instance-type table), plus the serializer paths for the members never emitted (defaultEnaQueueCountPerInterface, the missing interfaceTypeSet entry).

Cause 1 of 2: the c5ad family page. Network-card baselineBandwidthInGbps and peakBandwidthInGbps are the derived figures instead of the published ones, defaultEnaQueueCountPerInterface is absent, vCpuInfo.validCores is enumerated as every integer up to the vCPU count instead of the published valid-core list, and ebsInfo.ebsOptimizedInfo.baselineIops is the emulator default. Traces: ec2-catalog-describe-instance-types-a1-c9g#task#7.

Cause 2 of 2: the c5n family page. baselineBandwidthInGbps and peakBandwidthInGbps diverge for every size, defaultEnaQueueCountPerInterface is absent on all of them, and networkCards.item.interfaceTypeSet is missing the 'efa-only' entry on the EFA-capable sizes. Traces: ec2-catalog-describe-instance-types-a1-c9g#task#9.

Note for the fixer: the same catalog table backs 148 further unowned clusters in ec2-catalog-describe-instance-types-a1-c9g, -c9gd-m6gd, -m6i-r4 and -r9g-z1d; re-replay those fixtures end to end after correcting the table, not only the two steps named above.

## Evidence
Both clusters: expected status 200, actual status 200, verdict mismatch - the divergence is in the body.
Cause 1 (ec2-catalog-describe-instance-types-a1-c9g#task#7): instanceTypeSet.item[instanceType="c5ad.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps expected '10.0', got '8.33'; .defaultEnaQueueCountPerInterface expected '8', got None; .vCpuInfo.validCores.item[4] expected '8', got '5'; item[5] expected '12', got '6'; item[6] expected '16', got '7'; items[7..] expected absent, got '8','9','10',... (the dense enumeration); the same pattern repeats for c5ad.large/xlarge/2xlarge/4xlarge/12xlarge/16xlarge/24xlarge, and c5ad.4xlarge.ebsInfo.ebsOptimizedInfo.baselineIops also diverges.
Cause 2 (same fixture#task#9): c5n.2xlarge baselineBandwidthInGbps expected '10.0', got '11.11' and peakBandwidthInGbps expected '25.0', got '11.11'; c5n.xlarge expected '5.0'/'25.0', got '5.55'/'10.0'; c5n.large expected '3.0'/'25.0', got '2.77'/'10.0'; c5n.metal/.9xlarge/.18xlarge defaultEnaQueueCountPerInterface expected '32'/'8', got None; c5n.metal/.9xlarge/.18xlarge networkCards.item.interfaceTypeSet.item[2] expected 'efa-only', got None.
(The per-type numbers above are the published facts the recordings carry; the requirement is that the catalog serve each type's real values, not these literals for other types.)

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Replay ec2-catalog-describe-instance-types-a1-c9g step task#7 (c5ad page) and compare every networkCards member and vCpuInfo.validCores entry with the recording.
3. Replay step task#9 of the same fixture (c5n page) and compare networkCards bandwidths, defaultEnaQueueCountPerInterface and interfaceTypeSet.
4. Both return 200 with derived bandwidth figures, a dense validCores enumeration and the ENA queue count and 'efa-only' interface type absent.
5. Re-replay the other ec2-catalog-describe-instance-types-* fixtures to confirm the catalog correction covers the remaining clusters.
