Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-catalog-describe-instance-types-a1-c9g
Issue-Step: task#17
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-catalog-describe-instance-types-a1-c9g
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.12xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.16xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.24xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.24xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.24xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.2xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.32xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.32xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.32xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.32xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.48xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.48xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.48xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.48xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.48xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.4xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.8xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.large"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.medium"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.medium"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.medium"].networkInfo.networkCards.item.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.medium"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.medium"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.metal-48xl"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.metal-48xl"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.metal-48xl"].networkInfo.networkCards.item.interfaceTypeSet.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.metal-48xl"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.metal-48xl"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.12xlarge"].phcSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.12xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.16xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.16xlarge"].phcSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.16xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.2xlarge"].instanceStorageInfo.disks.item.sizeInGB
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.2xlarge"].instanceStorageInfo.totalSizeInGB
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.2xlarge"].phcSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.2xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.4xlarge"].phcSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.4xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.8xlarge"].phcSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.8xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.large"].phcSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.large"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.medium"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.medium"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.medium"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.medium"].phcSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.medium"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.metal"].autoRecoverySupported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.metal"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.metal"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.metal"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.metal"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.metal"].phcSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.metal"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.xlarge"].ebsInfo.ebsOptimizedInfo.baselineThroughputInMBps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.xlarge"].phcSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gd.xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.16xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.medium"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.medium"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.medium"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.metal"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.metal"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.metal"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.metal"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7gn.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.12xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.16xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.24xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.24xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.24xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.2xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.48xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.48xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.48xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.48xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.48xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.48xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.4xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.8xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.large"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.metal-24xl"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.metal-24xl"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.metal-24xl"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.metal-48xl"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.metal-48xl"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.metal-48xl"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.metal-48xl"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i.xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.12xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.16xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.2xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.4xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.8xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.large"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7i-flex.xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.24xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.24xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.24xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.48xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.48xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.48xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.48xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.48xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.medium"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.medium"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.medium"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.metal-24xl"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.metal-24xl"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.metal-24xl"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.metal-24xl"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.metal-48xl"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.metal-48xl"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.metal-48xl"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.metal-48xl"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8g.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.24xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.24xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.24xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.48xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.48xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.48xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.48xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.48xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.medium"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.medium"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.medium"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.metal-24xl"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.metal-24xl"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.metal-24xl"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.metal-24xl"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.metal-48xl"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.metal-48xl"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.metal-48xl"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.metal-48xl"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gd.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Related: 20261104T022117Z-ec2-describeinstancetypes-catalog-sweep-serves-derived-hardware-attributes.md
Issue-Commit: 4589a545c981676c72550ecd598553d4d546f08e

# ec2 DescribeInstanceTypes a1-c9g page: c7a through c8gd rows carry size-derived network, EBS and vCPU facts

> _Note: this issue was identified against the code base at commit `4589a545c981676c72550ecd598553d4d546f08e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceTypes

## Description
DescribeInstanceTypes answers 200 with a row whose per-type hardware facts are computed from the instance type's NAME (family letter + size suffix) instead of being the catalog row AWS publishes for that type. The handler's own doc states the design: "every member is derived by crate::state::instance_type_info from the type's name". Seven net-new divergence clusters on the a1-c9g catalog page are bundled here; each is one page of types whose rows diverge. The failure is always of three kinds: (a) a member AWS always emits is omitted (networkCards.defaultEnaQueueCountPerInterface, networkCards.interfaceTypeSet's 'efa-only' entry, networkInfo.ipv4AddressesPerSecondaryInterface), (b) a multi-valued member is emitted with only one entry (vCpuInfo.validCores, vCpuInfo.validThreadsPerCore, processorInfo.supportedFeatures), (c) a numeric or enum fact is a size formula's output rather than the published value (network-card baseline/peak bandwidth, EBS-optimized baseline/maximum Mbps/IOPS/MBps, maximumEbsAttachments, attachmentLimitType, rebootMigrationSupport, phcSupport, autoRecoverySupported, flexibleEnaQueuesSupport, secondaryNetworkSupported, vCpuInfo.defaultCores/defaultThreadsPerCore).

AWS (all causes): Smithy com.amazonaws.ec2 — DescribeInstanceTypes returns InstanceTypeInfoList (xmlName instanceTypeSet) with no modeled errors; NetworkCardInfo.DefaultEnaQueueCountPerInterface (integer, xmlName defaultEnaQueueCountPerInterface), NetworkCardInfo.BaselineBandwidthInGbps / PeakBandwidthInGbps (double), NetworkCardInfo.InterfaceTypes (list, xmlName interfaceTypeSet), VCpuInfo.ValidCores (CoreCountList) and ValidThreadsPerCore (ThreadsPerCoreList), ProcessorInfo.SupportedFeatures (SupportedAdditionalProcessorFeatureList), ProcessorInfo.SustainedClockSpeedInGhz (double). An instance type is a published FACT, so the per-type VALUES below are recorded behavior only: they are what the recorded AWS responses carry for those types, and AWS emits the same row for every caller.

Emulator (all causes): a 200 whose members are the name-derived values quoted per cause below; omitted members are absent from the XML entirely, and list members carry a single entry.

Why (all causes): DescribeInstanceTypes is the instance-type catalog clients read to pick a type, size an ENI/ENA budget, or decide whether a type supports EFA, PHC or reboot migration. A client reading this emulator picks types on fabricated bandwidth and core ladders, sees no defaultEnaQueueCountPerInterface where AWS always publishes one, and concludes a type does or does not support a capability that it in fact does; SDK consumers that iterate validCores see one value where AWS offers a whole ladder.

Fix (all causes): the instance-type catalog that backs the operation — crate::state::instance_type_info in the ec2 crate, which the handler reads — must serve a recorded per-type row (including the members it currently omits and the full lists) instead of deriving facts from the type name. Not the serializer: sibling list members do serialize correctly when they carry more than one entry.

Cause 1 of 7 — c7a.* rows (fixture ec2-catalog-describe-instance-types-a1-c9g, step task#17). AWS/Emulator: c7a.12xlarge ebsInfo.attachmentLimitType 'dedicated' -> 'shared'; ebsOptimizedInfo baselineBandwidthInMbps 15000 -> 10200, baselineIops 60000 -> 45000, baselineThroughputInMBps 1875.0 -> 1275.0, maximumBandwidthInMbps 15000 -> 10200, maximumIops 60000 -> 45000, maximumThroughputInMBps 1875.0 -> 1275.0, maximumEbsAttachments 32 -> 27; networkInfo.flexibleEnaQueuesSupport 'unsupported' -> 'supported'; networkInfo.ipv4AddressesPerSecondaryInterface 0 -> 30; networkCards.baselineBandwidthInGbps 18.75 -> 6.25; networkCards.defaultEnaQueueCountPerInterface omitted; vCpuInfo.validCores a ladder -> one entry; same shape of divergence on c7a.16xlarge, c7a.24xlarge, c7a.2xlarge, c7a.32xlarge, c7a.48xlarge.

Cause 2 of 7 — c7gd.* rows (step task#19). c7gd.medium networkCards.baselineBandwidthInGbps 0.52 -> 0.39, peakBandwidthInGbps 12.5 -> 10.0, defaultEnaQueueCountPerInterface 1 omitted, phcSupport 'supported' -> 'unsupported', rebootMigrationSupport 'unsupported' -> 'supported'; c7gd.xlarge ebsOptimizedInfo.baselineThroughputInMBps 156.25039999999998 -> 156.25, baselineBandwidthInGbps 1.876 -> 1.56; c7gd.metal autoRecoverySupported diverges; c7gd.2xlarge instanceStorageInfo.disks.sizeInGB and totalSizeInGB diverge; c7gd.16xlarge/metal networkCards.interfaceTypeSet's third entry omitted.

Cause 3 of 7 — c7gn.* rows (step task#20). c7gn.metal networkCards.baselineBandwidthInGbps 200.0 -> 100.0, peakBandwidthInGbps 200.0 -> 100.0, defaultEnaQueueCountPerInterface 32 omitted, interfaceTypeSet entry 'efa-only' omitted; c7gn.2xlarge baseline 25.0 -> 12.5 and peak 50.0 -> 12.5; c7gn.8xlarge 100.0 -> 50.0; c7gn.xlarge 12.5 -> 6.25; every c7gn size omits defaultEnaQueueCountPerInterface.

Cause 4 of 7 — c7i.* rows (step task#21). c7i.48xlarge networkCards.baselineBandwidthInGbps 50.0 -> 25.0, peakBandwidthInGbps 50.0 -> 25.0, defaultEnaQueueCountPerInterface 16 omitted, interfaceTypeSet entry 'efa-only' omitted, processorInfo.supportedFeatures ['nested-virtualization','metal-compatibility'] -> a single 'nested-virtualization', vCpuInfo.validCores the ladder 4,6,...,96 -> the single value 96; c7i.metal-48xl and the rest of the family diverge the same way.

Cause 5 of 7 — c7i-flex.* rows (step task#22). c7i-flex.12xlarge networkCards.baselineBandwidthInGbps 9.375 -> 18.75, defaultEnaQueueCountPerInterface 4 omitted, processorInfo.supportedFeatures ['nested-virtualization','metal-compatibility'] -> 'nested-virtualization'; c7i-flex.8xlarge 6.25 -> 12.5; c7i-flex.2xlarge 1.562 -> 3.12 with peakBandwidthInGbps 12.5 -> 10.0; same for large/xlarge/4xlarge/16xlarge.

Cause 6 of 7 — c8g.* rows (step task#24). c8g.metal-24xl networkCards.baselineBandwidthInGbps 40.0 -> 12.5 and peakBandwidthInGbps 40.0 -> 12.5, defaultEnaQueueCountPerInterface 32 omitted, interfaceTypeSet 'efa-only' omitted; c8g.metal-48xl 50.0 -> 25.0; c8g.8xlarge 15.0 -> 4.16 baseline and 15.0 -> 10.0 peak; c8g.48xlarge vCpuInfo.validCores collapsed.

Cause 7 of 7 — c8gd.* rows (step task#26). c8gd.large networkCards.baselineBandwidthInGbps 0.937 -> 0.26, peakBandwidthInGbps 12.5 -> 10.0, defaultEnaQueueCountPerInterface 2 omitted; c8gd.xlarge 1.875 -> 0.52; c8gd.2xlarge 3.75 -> 1.04 with peak 15.0 -> 10.0; c8gd.24xlarge 40.0 -> 12.5; c8gd.48xlarge validCores collapsed and interfaceTypeSet 'efa-only' omitted.

Related open issues that do NOT carry these clusters' replay keys (the replay reports no owner for all seven): 20261117T072456Z-ec2-describeinstancetypes-a1-c9g-page-rows-carry-formula.md, 20261114T013533Z-ec2-describeinstancetypes-serves-size-derived-hardware-facts-across.md, 20261109T031226Z-ec2-catalog-pages-a1-c9g-and-c9gd-m6gd.md.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator; 7 unowned divergence clusters, all verdict=mismatch, expected_status 200 / actual_status 200 (the divergence is in the body).

Representative (ec2-catalog-describe-instance-types-a1-c9g#task#17, DescribeInstanceTypes):
  $.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7a.12xlarge"].ebsInfo.attachmentLimitType: expected 'dedicated', got 'shared'
  ...ebsInfo.ebsOptimizedInfo.baselineBandwidthInMbps: expected '15000', got '10200'
  ...ebsInfo.ebsOptimizedInfo.baselineIops: expected '60000', got '45000'
  ...ebsInfo.ebsOptimizedInfo.baselineThroughputInMBps: expected '1875.0', got '1275.0'
  ...ebsInfo.maximumEbsAttachments: expected '32', got '27'
  ...networkInfo.flexibleEnaQueuesSupport: expected 'unsupported', got 'supported'
  ...networkInfo.ipv4AddressesPerSecondaryInterface: expected '0', got '30'
  ...networkInfo.networkCards.item.baselineBandwidthInGbps: expected '18.75', got '6.25'
  ...networkInfo.networkCards.item.defaultEnaQueueCountPerInterface: missing (expected a value, got None)
  ...vCpuInfo.validCores.item[6..23]: missing

Other bundled causes (one fixture#step each, same operation):
  a1-c9g#task#19 c7gd: baselineBandwidthInGbps expected '0.52' got '0.39'; defaultEnaQueueCountPerInterface expected '1' got None; phcSupport expected 'supported' got 'unsupported'; rebootMigrationSupport expected 'unsupported' got 'supported'
  a1-c9g#task#20 c7gn: c7gn.metal baseline/peak expected '200.0' got '100.0'; defaultEnaQueueCountPerInterface expected '32' got None; interfaceTypeSet.item[2] expected 'efa-only' got None
  a1-c9g#task#21 c7i: c7i.48xlarge baseline/peak expected '50.0' got '25.0'; processorInfo.supportedFeatures expected ['nested-virtualization','metal-compatibility'] got 'nested-virtualization'; vCpuInfo.validCores expected ['4','6',...,'96'] got '96'
  a1-c9g#task#22 c7i-flex: baselineBandwidthInGbps expected '9.375' got '18.75' (12xlarge), '1.562' got '3.12' (2xlarge); defaultEnaQueueCountPerInterface expected '4' got None
  a1-c9g#task#24 c8g: c8g.metal-24xl baseline/peak expected '40.0' got '12.5'; c8g.8xlarge baseline expected '15.0' got '4.16'; defaultEnaQueueCountPerInterface expected '32' got None
  a1-c9g#task#26 c8gd: c8gd.large baseline expected '0.937' got '0.26'; c8gd.24xlarge baseline expected '40.0' got '12.5'; defaultEnaQueueCountPerInterface expected '2' got None

Model grounding: services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json — NetworkCardInfo.DefaultEnaQueueCountPerInterface/BaselineBandwidthInGbps/PeakBandwidthInGbps/InterfaceTypes, VCpuInfo.ValidCores/ValidThreadsPerCore, ProcessorInfo.SupportedFeatures/SustainedClockSpeedInGhz. Emulator source: services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_instance_types.rs lines 10-16 ("every member is derived by crate::state::instance_type_info from the type's name").

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture ec2-catalog-describe-instance-types-a1-c9g.
2. At step task#17 the fixture calls DescribeInstanceTypes for the c7a sizes; compare the returned instanceTypeSet rows with the recorded response: attachmentLimitType, every ebsOptimizedInfo number, maximumEbsAttachments, flexibleEnaQueuesSupport, ipv4AddressesPerSecondaryInterface, networkCards.baselineBandwidthInGbps and vCpuInfo.validCores diverge, and networkCards.defaultEnaQueueCountPerInterface is absent.
3. Repeat for steps task#19 (c7gd), task#20 (c7gn), task#21 (c7i), task#22 (c7i-flex), task#24 (c8g) and task#26 (c8gd) of the same fixture: each page diverges on the same classes of member.
4. Equivalently, call DescribeInstanceTypes with InstanceTypes=c7i.48xlarge and observe that vCpuInfo.validCores carries one entry and processorInfo.supportedFeatures one entry.
