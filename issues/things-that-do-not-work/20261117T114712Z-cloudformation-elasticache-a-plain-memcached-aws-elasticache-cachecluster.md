Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__elasticache.deploy-memcached-cachecluster-configuration-endpoint-snapshot-retention-rejected
Issue-Key: cloudformation__elasticache.deploy-memcached-cachecluster-configuration-endpoint-snapshot-retention-rejected|run#7|AssertionError|assert status == "CREATE_COMPLETE", (
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# cloudformation__elasticache: a plain memcached AWS::ElastiCache::CacheCluster in a stack-created VPC subnet group ends CREATE_FAILED, rolling the whole stack back

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: the CloudFormation resource provider for AWS::ElastiCache::CacheCluster cannot create a one-node memcached cluster, so a well-formed control stack rolls back and the GetAtt ConfigurationEndpoint.Address projection can never be observed.

AWS: a stack of AWS::EC2::VPC + two AWS::EC2::Subnet + AWS::EC2::SecurityGroup + AWS::ElastiCache::SubnetGroup + AWS::ElastiCache::CacheCluster{Engine=memcached, CacheNodeType=cache.t3.micro, NumCacheNodes=1, CacheSubnetGroupName=Ref SubnetGroup, VpcSecurityGroupIds=[Ref SecurityGroup]} reaches StackStatus CREATE_COMPLETE with every resource CREATE_COMPLETE, and the stack's Outputs carry Ref Cluster plus Fn::GetAtt [Cluster, "ConfigurationEndpoint.Address"], whose value equals the ConfigurationEndpoint.Address that ElastiCache's own DescribeCacheClusters(ShowCacheNodeInfo=true) reports for that cluster (Engine "memcached"). Recorded behavior only for the per-resource sequencing; the ElastiCache side is the modeled CreateCacheCluster for a memcached engine, which this template exercises with nothing unusual.
Emulator: the Cluster resource goes CREATE_FAILED and CloudFormation rolls the stack back — the recorded event sequence is Vpc/SubnetA/SubnetB/SecurityGroup/SubnetGroup CREATE_COMPLETE, then ('Cluster','CREATE_FAILED'), then the full delete chain down to ('<stack>','ROLLBACK_COMPLETE'). The assertion printed only the decisive (LogicalResourceId, ResourceStatus) rows, so the handler's ResourceStatusReason for the Cluster failure is not in the replay output and must be read out of DescribeStackEvents when reproducing.
Why: no client can provision an ElastiCache cache cluster through CloudFormation at all — the canonical memcached-in-a-VPC template rolls back, taking the VPC, subnets, security group and subnet group with it, and any template whose Outputs or dependents reference the cluster's configuration endpoint is unusable. It also means the negative half of this scenario (a memcached CacheCluster carrying SnapshotRetentionLimit=5, which AWS refuses with HandlerErrorCode InvalidRequest and the handler message "Engine does not support snapshotting. Snapshot retention limit parameter should not be specified.") is never graded, because the positive control fails first.
Fix: the CloudFormation AWS::ElastiCache::CacheCluster resource provider's create path — the mapping of the template's ClusterName/Engine/CacheNodeType/NumCacheNodes/CacheSubnetGroupName/VpcSecurityGroupIds properties onto elasticache CreateCacheCluster and the stabilization that follows it — plus the ConfigurationEndpoint.Address GetAtt projection that depends on it. Related but distinct, and not covering this check: open issue 20261112T190032Z-elasticache-migration-and-global-datastore-state-machine-diverge.md reports a CFN CacheCluster stack rollback under the `elasticache` suite heading, and 20261109T151249Z-cloudformation-elasticache-resources-two-types-are-unrecognized-and.md covers unrecognized ElastiCache resource types and SubnetGroup/ParameterGroup/User reaching CREATE_COMPLETE without the underlying resource — neither owns this memcached CacheCluster create failure.
Traces: cloudformation__elasticache.deploy-memcached-cachecluster-configuration-endpoint-snapshot-retention-rejected.

## Evidence
Check cloudformation__elasticache.deploy-memcached-cachecluster-configuration-endpoint-snapshot-retention-rejected:
  expected: status == "CREATE_COMPLETE" for the control stack; Outputs.ConfigurationEndpointAddress == DescribeCacheClusters(...).CacheClusters[0].ConfigurationEndpoint.Address; Engine == "memcached"
  actual:   AssertionError: control stack ended 'ROLLBACK_COMPLETE': [('Vpc', 'CREATE_COMPLETE'), ('SubnetA', 'CREATE_COMPLETE'), ('SubnetB', 'CREATE_COMPLETE'), ('SecurityGroup', 'CREATE_COMPLETE'), ('SubnetGroup', 'CREATE_COMPLETE'), ('Cluster', 'CREATE_FAILED'), ('Cluster', 'DELETE_COMPLETE'), ('SubnetGroup', 'DELETE_COMPLETE'), ('SecurityGroup', 'DELETE_COMPLETE'), ('SubnetB', 'DELETE_COMPLETE'), ('SubnetA', 'DELETE_COMPLETE'), ('Vpc', 'DELETE_COMPLETE'), ('<stack-name>', 'ROLLBACK_COMPLETE')]
  (the stack name and resource physical ids are per-run values, not requirements)
Not reached because of the above, and therefore not asserted here: the SnapshotRetentionLimit=5 variant's required handler reason ('Engine does not support snapshotting. Snapshot retention limit parameter should not be specified.', HandlerErrorCode InvalidRequest) and the CacheClusterNotFound follow-ups.

## Steps to Reproduce
1. CreateStack with a template containing: AWS::EC2::VPC (CidrBlock 10.213.0.0/16), two AWS::EC2::Subnet (10.213.1.0/24, 10.213.2.0/24, AvailabilityZone from Fn::Select/Fn::GetAZs), AWS::EC2::SecurityGroup, AWS::ElastiCache::SubnetGroup (SubnetIds = both subnets), and AWS::ElastiCache::CacheCluster with Properties {ClusterName, Engine: memcached, CacheNodeType: cache.t3.micro, NumCacheNodes: 1, CacheSubnetGroupName: {Ref: SubnetGroup}, VpcSecurityGroupIds: [{Ref: SecurityGroup}]}; Outputs: Ref Cluster and Fn::GetAtt [Cluster, ConfigurationEndpoint.Address].
2. Poll DescribeStacks(StackName=<StackId>) until terminal. The emulator reaches ROLLBACK_COMPLETE; AWS reaches CREATE_COMPLETE.
3. DescribeStackEvents(StackName=<StackId>) and read the Cluster CREATE_FAILED row's ResourceStatusReason to see why the provider refused.
4. After the fix, compare Outputs.ConfigurationEndpointAddress against elasticache DescribeCacheClusters(CacheClusterId=<ClusterName>, ShowCacheNodeInfo=true).CacheClusters[0].ConfigurationEndpoint.Address, then re-run the SnapshotRetentionLimit=5 variant and check its rollback reason.
