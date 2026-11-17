Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-catalog-describe-instance-types-a1-c9g
Issue-Step: task#12
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-catalog-describe-instance-types-a1-c9g
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.12xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.8xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.medium"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.medium"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.medium"].networkInfo.networkCards.item.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.metal"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gd.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.16xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.2xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.large"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.medium"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.medium"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.medium"].networkInfo.networkCards.item.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.medium"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.medium"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6gn.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Related: 20261102T023824Z-ec2-describeinstancetypes-multi-type-responses-omit-modeled-members.md
Issue-Commit: 0e5c96f71ec439fc5dd27026803182d983843d39

# ec2 DescribeInstanceTypes a1-c9g page rows carry formula-derived EBS and network-card facts

> _Note: this issue was identified against the code base at commit `0e5c96f71ec439fc5dd27026803182d983843d39`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceTypes

## Description
Two net-new wire causes, one per catalog page, in the EC2 emulator's DescribeInstanceTypes catalog. Both must be closed before this issue is submitted. The shared fault: the per-type hardware facts are computed from the instance size (a vCPU/size multiplier formula) instead of being served from AWS's published per-type catalog, and one modeled member is omitted entirely.

Cause 1 of 2: the c6gd family page serves derived EBS-optimized, network-card and migration facts and omits defaultEnaQueueCountPerInterface.
AWS: Smithy (com.amazonaws.ec2) DescribeInstanceTypes -> InstanceTypeInfo. EbsOptimizedInfo (xmlName ebsOptimizedInfo) carries baselineBandwidthInMbps (integer, 'The baseline bandwidth performance for an EBS-optimized instance type, in Mbps'), baselineIops, baselineThroughputInMBps, maximumBandwidthInMbps, maximumIops, maximumThroughputInMBps; NetworkInfo$NetworkCards -> NetworkCardInfo carries baselineBandwidthInGbps, peakBandwidthInGbps, networkPerformance, maximumNetworkInterfaces and defaultEnaQueueCountPerInterface ('The default number of the ENA queues for each interface'); InstanceTypeInfo also carries rebootMigrationSupport, enaSrdSupported, nitroEnclavesSupport, trafficMirroringSupport, currentGeneration and instanceStorageInfo.disks.item.sizeInGB/totalSizeInGB. These are published hardware facts of each instance type: AWS reports one fixed value per instance type, they are not a function of the type's size, and defaultEnaQueueCountPerInterface is present on every network card AWS returns for these types. The exact numbers per type are recorded behavior (the fixture holds AWS's values for the whole page).
Emulator: the page answers 200 but nearly every per-type attribute is a scaled value. For c6gd.xlarge it serves ebsOptimizedInfo.baselineBandwidthInMbps 1150 where AWS reports 1188, baselineThroughputInMBps 143.75 vs 148.5, maximumIops 18750 vs 20000, networkCards.item.baselineBandwidthInGbps 1.56 vs 1.25, enaSrdSupported 'true' vs 'false' and rebootMigrationSupport 'supported' vs 'unsupported', and it omits networkCards.item.defaultEnaQueueCountPerInterface entirely (AWS: 8). For c6gd.2xlarge the same members are off (2300 vs 2375, 287.5 vs 296.875, 18750 vs 20000) and instanceStorageInfo.disks.item.sizeInGB is 475 vs AWS's 474. The same shape of error repeats for c6gd.large/medium/4xlarge/8xlarge/12xlarge/16xlarge/metal in one response, which is what identifies the source as a size formula rather than per-type data.
Why: this catalog is what clients use to choose and validate instance types. Capacity planners that read ebsOptimizedInfo or networkCards bandwidth size storage and network budgets from numbers AWS never publishes; a client that gates on rebootMigrationSupport or enaSrdSupported takes the opposite branch; and code that reads networkCards.item.defaultEnaQueueCountPerInterface -- a member AWS always emits for these types -- gets no value at all and either crashes on the missing key or silently falls back to a default.
Fix: the DescribeInstanceTypes handler's static instance-type catalog (the data table it projects, not the serializer) must carry AWS's published per-type rows for this page, including defaultEnaQueueCountPerInterface on every network card, instead of deriving attributes from the size suffix.
Traces: ec2-catalog-describe-instance-types-a1-c9g#task#12.

Cause 2 of 2: the c6gn family page serves derived values and additionally mis-stamps currentGeneration and the interface counts.
AWS: same shapes as Cause 1, plus InstanceTypeInfo$currentGeneration and NetworkInfo$ipv4AddressesPerInterface / ipv6AddressesPerInterface / maximumNetworkInterfaces. The c6gn rows AWS serves in this page report currentGeneration 'false', and the network-interface and per-interface address limits are per-type facts.
Emulator: for c6gn.medium it serves currentGeneration 'true' vs AWS's 'false', ebsOptimizedInfo.baselineBandwidthInMbps 287 vs 760, baselineIops 1500 vs 2500, baselineThroughputInMBps 35.875 vs 95.0, maximumBandwidthInMbps 4750 vs 9500, maximumIops 18750 vs 40000, maximumThroughputInMBps 593.75 vs 1187.5, networkInfo.ipv4AddressesPerInterface and ipv6AddressesPerInterface 6 vs 4, maximumNetworkInterfaces 3 vs 2, enaSrdSupported 'true' vs 'false' and networkCards.item.baselineBandwidthInGbps 1.56 vs 1.6, and again omits defaultEnaQueueCountPerInterface; the whole c6gn page (large through 16xlarge) diverges the same way.
Why: in addition to the sizing consequences above, currentGeneration is the flag clients use to filter out previous-generation types, and the interface/address limits are what ENI-placement code budgets against -- an over-reported maximumNetworkInterfaces lets a client plan an attachment AWS would refuse.
Fix: as Cause 1, for this page's rows.

Scope note for the fixer: this same root cause is reported by the replay as 143 unowned DescribeInstanceTypes clusters across the ec2-catalog-describe-instance-types-* fixtures; only the two pages named above are bound to this issue's replay pointer, so re-replaying those two steps confirms the pages above and not the whole catalog. Existing open issues describe the same fault for other pages but do not carry these clusters' replay keys: 20261114T013533Z-ec2-describeinstancetypes-serves-size-derived-hardware-facts-across.md, 20261112T231741Z-ec2-describeinstancetypes-catalog-rows-are-synthesized-from-a.md, 20261111T015735Z-ec2-describeinstancetypes-catalog-pages-c9gd-m6gd-and-m6i.md and 20261117T065015Z-ec2-describeinstancetypes-serves-synthesized-instance-type-catalog-rows.md.

## Evidence
Cause 1 — fixture ec2-catalog-describe-instance-types-a1-c9g, step task#12, DescribeInstanceTypes, expected status 200, actual status 200, verdict mismatch (value_mismatch/missing on the page's rows):
  $...item[instanceType="c6gd.xlarge"].ebsInfo.ebsOptimizedInfo.baselineBandwidthInMbps: expected '1188', got '1150'
  $...item[instanceType="c6gd.xlarge"].ebsInfo.ebsOptimizedInfo.baselineThroughputInMBps: expected '148.5', got '143.75'
  $...item[instanceType="c6gd.xlarge"].ebsInfo.ebsOptimizedInfo.maximumIops: expected '20000', got '18750'
  $...item[instanceType="c6gd.xlarge"].networkInfo.enaSrdSupported: expected 'false', got 'true'
  $...item[instanceType="c6gd.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps: expected '1.25', got '1.56'
  $...item[instanceType="c6gd.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface: missing (expected '8', got None)
  $...item[instanceType="c6gd.xlarge"].rebootMigrationSupport: expected 'unsupported', got 'supported'
  $...item[instanceType="c6gd.2xlarge"].ebsInfo.ebsOptimizedInfo.baselineBandwidthInMbps: expected '2375', got '2300'
  $...item[instanceType="c6gd.2xlarge"].ebsInfo.ebsOptimizedInfo.baselineThroughputInMBps: expected '296.875', got '287.5'
  $...item[instanceType="c6gd.2xlarge"].ebsInfo.ebsOptimizedInfo.maximumIops: expected '20000', got '18750'
  $...item[instanceType="c6gd.2xlarge"].instanceStorageInfo.disks.item.sizeInGB: expected '474', got '475'
  (the cluster's key list repeats the same members for c6gd.large, c6gd.medium, c6gd.4xlarge, c6gd.8xlarge, c6gd.12xlarge, c6gd.16xlarge and c6gd.metal, including maximumEbsAttachments and trafficMirroringSupport on c6gd.metal and nitroEnclavesSupport on c6gd.large)

Cause 2 — fixture ec2-catalog-describe-instance-types-a1-c9g, step task#13, DescribeInstanceTypes, expected status 200, actual status 200, verdict mismatch:
  $...item[instanceType="c6gn.medium"].currentGeneration: expected 'false', got 'true'
  $...item[instanceType="c6gn.medium"].ebsInfo.ebsOptimizedInfo.baselineBandwidthInMbps: expected '760', got '287'
  $...item[instanceType="c6gn.medium"].ebsInfo.ebsOptimizedInfo.baselineIops: expected '2500', got '1500'
  $...item[instanceType="c6gn.medium"].ebsInfo.ebsOptimizedInfo.baselineThroughputInMBps: expected '95.0', got '35.875'
  $...item[instanceType="c6gn.medium"].ebsInfo.ebsOptimizedInfo.maximumBandwidthInMbps: expected '9500', got '4750'
  $...item[instanceType="c6gn.medium"].ebsInfo.ebsOptimizedInfo.maximumIops: expected '40000', got '18750'
  $...item[instanceType="c6gn.medium"].ebsInfo.ebsOptimizedInfo.maximumThroughputInMBps: expected '1187.5', got '593.75'
  $...item[instanceType="c6gn.medium"].networkInfo.enaSrdSupported: expected 'false', got 'true'
  $...item[instanceType="c6gn.medium"].networkInfo.ipv4AddressesPerInterface: expected '4', got '6'
  $...item[instanceType="c6gn.medium"].networkInfo.ipv6AddressesPerInterface: expected '4', got '6'
  $...item[instanceType="c6gn.medium"].networkInfo.maximumNetworkInterfaces: expected '2', got '3'
  $...item[instanceType="c6gn.medium"].networkInfo.networkCards.item.baselineBandwidthInGbps: expected '1.6', got '1.56'
  (the cluster's key list repeats the same members, plus networkCards.item.defaultEnaQueueCountPerInterface as missing, for c6gn.large, c6gn.xlarge, c6gn.2xlarge, c6gn.4xlarge, c6gn.8xlarge, c6gn.12xlarge and c6gn.16xlarge)

In both clusters the status and the response envelope are correct — only the per-type catalog values diverge, and the pattern (every size in a family off by a constant ratio, one member absent) is what shows the rows are computed rather than served from AWS's catalog.

## Steps to Reproduce
1. Replay wire fixture 'ec2-catalog-describe-instance-types-a1-c9g' (trace set simparity-wire-aws-v1) against the emulator.
2. Step task#12 — DescribeInstanceTypes for the c6gd family page (the fixture requests the page's instance types / filters for that family) and compare every instanceTypeSet.item against the recorded response: ebsInfo.ebsOptimizedInfo.*, networkInfo.networkCards.item.*, networkInfo.enaSrdSupported, rebootMigrationSupport, instanceStorageInfo.* .
3. Step task#13 — the same call for the c6gn family page; additionally compare currentGeneration, networkInfo.ipv4AddressesPerInterface / ipv6AddressesPerInterface and networkInfo.maximumNetworkInterfaces.
4. Both steps answer HTTP 200 and are reported as verdict 'mismatch' by replay_wire_corpus, with the per-type members listed under Evidence; networkCards.item.defaultEnaQueueCountPerInterface is absent from every row in both pages.
