Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-catalog-describe-instance-types-a1-c9g
Issue-Step: task#29
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-catalog-describe-instance-types-a1-c9g ec2-catalog-describe-instance-types-c9gd-m6gd
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.12xlarge"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.12xlarge"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.12xlarge"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.12xlarge"].networkInfo.networkCards.item.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.12xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.16xlarge"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.16xlarge"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.16xlarge"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.16xlarge"].networkInfo.networkCards.item.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.16xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.2xlarge"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.2xlarge"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.2xlarge"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.2xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.4xlarge"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.4xlarge"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.4xlarge"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.4xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.8xlarge"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.8xlarge"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.8xlarge"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.8xlarge"].networkInfo.networkCards.item.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.8xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.large"].ebsInfo.ebsOptimizedInfo.baselineBandwidthInMbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.large"].ebsInfo.ebsOptimizedInfo.baselineIops
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.large"].ebsInfo.ebsOptimizedInfo.baselineThroughputInMBps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.large"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.large"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.large"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.large"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.xlarge"].ebsInfo.ebsOptimizedInfo.baselineBandwidthInMbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.xlarge"].ebsInfo.ebsOptimizedInfo.baselineThroughputInMBps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.xlarge"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.xlarge"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.xlarge"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8i-flex.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.12xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.16xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.8xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.metal"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.24xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.24xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.24xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m5a.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Related: 20261029T215142Z-ec2-describe-projections-omit-modeled-members-and-seed.md
Issue-Related: 20261109T031226Z-ec2-catalog-pages-a1-c9g-and-c9gd-m6gd.md
Issue-Commit: 94f7b27553b727586aac9de0add3c17de28dbaa9

# ec2 DescribeInstanceTypes rows are formula-derived instead of EC2's published catalog

> _Note: this issue was identified against the code base at commit `94f7b27553b727586aac9de0add3c17de28dbaa9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceTypes

## Description
Three net-new causes (three replay clusters), one fault: DescribeInstanceTypes computes each row's hardware facts from the instance-type NAME (size multiplier x a per-family formula) instead of serving EC2's published per-type catalog, and it omits the ENA queue members entirely. Authority is "recorded behavior only": the Smithy model defines the members (NetworkCardInfo, EbsOptimizedInfo, VCpuInfo.ValidCores, PlacementGroupInfo.SupportedStrategies, ProcessorInfo, NitroEnclavesSupport, RebootMigrationSupport, FreeTierEligible) but not their per-type values, so the recordings are the reference. Every value quoted below is one recording's example of the general rule — the requirement is that each type's row matches the published catalog for that type, not these particular numbers.

Cause 1 of 3 — the c7a..c8i-flex rows of the a1-c9g page (fixture ec2-catalog-describe-instance-types-a1-c9g, step task#29).
AWS: recorded behavior only — per-type published values, e.g. for a c8i-flex size: networkInfo.ipv4AddressesPerInterface/ipv6AddressesPerInterface 50 with ipv4AddressesPerSecondaryInterface 0, maximumNetworkInterfaces 10, enaSrdSupported false, and networkCards entries carrying additionalFlexibleNetworkInterfaces, defaultEnaQueueCountPerInterface, maximumEnaQueueCount and maximumEnaQueueCountPerInterface.
Emulator: serves size-scaled values instead (30 addresses per interface for every interface class, 8 network interfaces, enaSrdSupported true) and omits the four ENA-queue/flexible-interface members (got None), plus derived baselineBandwidthInGbps/peakBandwidthInGbps, networkPerformance strings, placementGroupInfo.supportedStrategies, processorInfo.sustainedClockSpeedInGhz/supportedFeatures and nitroEnclavesSupport.
Why: capacity planners and schedulers read these numbers to decide how many ENIs/IPs/queues a type can carry; a derived row makes a type look larger or smaller than it is, and a member that is absent cannot be read at all.
Fix: the DescribeInstanceTypes read path and the instance-type catalog data it serves (the per-family formula must be replaced by published rows, including the ENA queue members).

Cause 2 of 3 — the m5 family rows of the c9gd-m6gd page (fixture ec2-catalog-describe-instance-types-c9gd-m6gd, step task#35).
AWS: recorded behavior only — m5 is not a current-generation family (currentGeneration false); vCpuInfo.validCores lists the selectable core counts in steps of the type's threads-per-core (2,4,6,8,…), networkPerformance for a fixed-bandwidth size is the plain string "10 Gigabit" (no "Up to" prefix), and networkCards baselineBandwidthInGbps/defaultEnaQueueCountPerInterface are published per type.
Emulator: reports currentGeneration true, validCores as a plain 1..n sequence, networkPerformance "Up to 10 Gigabit" for a dedicated-bandwidth size, derived baselineBandwidthInGbps (8.33 where AWS publishes 10.0), and omits defaultEnaQueueCountPerInterface; the same row set also diverges on ebsInfo.ebsOptimizedInfo baseline/maximum bandwidth, IOPS and throughput, autoRecoverySupported, trafficMirroringSupport, rebootMigrationSupport, maximumEbsAttachments and supportedBootModes for m5.metal.
Why: currentGeneration and validCores drive type-selection and CPU-options validation; "Up to" vs fixed bandwidth is how a client tells burstable from dedicated network performance.
Fix: same component — the instance-type catalog data and the DescribeInstanceTypes projection that fills validCores/currentGeneration/networkPerformance.

Cause 3 of 3 — the m5a family rows of the same page (fixture ec2-catalog-describe-instance-types-c9gd-m6gd, step task#36).
AWS: recorded behavior only — published per-type network-card bandwidths (baseline and peak equal for a dedicated-bandwidth size), defaultEnaQueueCountPerInterface present on every network card, and vCpuInfo.validCores as a LIST of core counts even for the largest size.
Emulator: derives bandwidths (12.5 where AWS publishes 10.0; 4.16 where AWS publishes 5.0), omits defaultEnaQueueCountPerInterface, and collapses validCores to a single scalar instead of the list (type_mismatch: expected ['12','18','24','36','48'], got '48').
Why: a scalar where the model declares a list breaks SDK deserialization of ValidCores, and the derived bandwidths misreport the type's network capability.
Fix: same component — the catalog data plus the serializer's single-element/list handling for validCores.

Many further DescribeInstanceTypes clusters in this corpus share this root cause; the open issues 20261111T180943Z-ec2-read-paths-serve-emulator-derived-catalog-data.md, 20261112T182924Z-ec2-describeinstancetypes-rows-for-the-c4-c9gd-m6i.md, 20261120T225613Z-ec2-describeinstancetypes-a1-c9g-page-c7a-through-c8gd.md and 20261123T015522Z-ec2-describeinstancetypes-serves-synthesized-instance-type-metadata-instead.md cover neighbouring pages but do NOT carry the three replay keys bound to this issue.

## Evidence
Cause 1 — ec2-catalog-describe-instance-types-a1-c9g task#29, status 200 vs 200: item[instanceType="c8i-flex.8xlarge"].networkInfo.enaSrdSupported expected 'false' got 'true'; ipv4AddressesPerInterface expected '50' got '30'; ipv4AddressesPerSecondaryInterface expected '0' got '30'; maximumNetworkInterfaces expected '10' got '8'; networkCards.item.additionalFlexibleNetworkInterfaces expected '0' got None; defaultEnaQueueCountPerInterface expected '4' got None; maximumEnaQueueCount expected '64' got None; maximumEnaQueueCountPerInterface expected '16' got None; baselineBandwidthInGbps expected '7.5' got '12.5'; the same divergences repeat across c8i-flex.large/xlarge/2xlarge/4xlarge/12xlarge/16xlarge plus placementGroupInfo.supportedStrategies, processorInfo.* and nitroEnclavesSupport.
Cause 2 — ec2-catalog-describe-instance-types-c9gd-m6gd task#35, status 200 vs 200: item[instanceType="m5.8xlarge"].currentGeneration expected 'false' got 'true'; networkCards.item.baselineBandwidthInGbps expected '10.0' got '8.33'; defaultEnaQueueCountPerInterface expected '8' got None; networkCards.item.networkPerformance and networkInfo.networkPerformance expected '10 Gigabit' got 'Up to 10 Gigabit'; vCpuInfo.validCores.item[0..] expected 2,4,6,8,10,12,14,… got 1,2,3,4,5,6,7,…; m5.metal rows additionally diverge on autoRecoverySupported, maximumEbsAttachments, trafficMirroringSupport, rebootMigrationSupport and supportedBootModes.
Cause 3 — ec2-catalog-describe-instance-types-c9gd-m6gd task#36, status 200 vs 200: item[instanceType="m5a.12xlarge"].networkCards.item.baselineBandwidthInGbps and peakBandwidthInGbps expected '10.0' got '12.5'; m5a.4xlarge baselineBandwidthInGbps expected '5.0' got '4.16'; defaultEnaQueueCountPerInterface expected '8' got None on every m5a size; item[instanceType="m5a.24xlarge"].vCpuInfo.validCores.item type_mismatch (expected ['12','18','24','36','48'], got '48').

## Steps to Reproduce
Replay trace set simparity-wire-aws-v1 and inspect ec2-catalog-describe-instance-types-a1-c9g task#29 and ec2-catalog-describe-instance-types-c9gd-m6gd task#35 and task#36. Equivalently: call DescribeInstanceTypes for the c8i-flex, m5 and m5a families and compare networkInfo (address counts, maximumNetworkInterfaces, enaSrdSupported, networkCards incl. the ENA queue members), ebsInfo.ebsOptimizedInfo, networkPerformance strings, currentGeneration, vCpuInfo.validCores and placementGroupInfo.supportedStrategies against EC2's published per-type values.
