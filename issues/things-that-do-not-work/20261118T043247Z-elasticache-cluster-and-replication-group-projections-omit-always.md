Issue-Source: trace-sweep-divergence
Issue-Trace: elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found
Issue-Step: task#16
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found elasticache-replication-group-explicit-port-read-back-tags-and-not-found
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache|CreateCacheCluster|2xx->2xx|$.CreateCacheClusterResponse.CreateCacheClusterResult.CacheCluster.AtRestEncryptionEnabled
Issue-Key: elasticache|CreateCacheCluster|2xx->2xx|$.CreateCacheClusterResponse.CreateCacheClusterResult.CacheCluster.AutoMinorVersionUpgrade
Issue-Key: elasticache|CreateCacheCluster|2xx->2xx|$.CreateCacheClusterResponse.CreateCacheClusterResult.CacheCluster.CacheParameterGroup.CacheNodeIdsToReboot
Issue-Key: elasticache|CreateCacheCluster|2xx->2xx|$.CreateCacheClusterResponse.CreateCacheClusterResult.CacheCluster.CacheSecurityGroups
Issue-Key: elasticache|CreateCacheCluster|2xx->2xx|$.CreateCacheClusterResponse.CreateCacheClusterResult.CacheCluster.ClientDownloadLandingPage
Issue-Key: elasticache|CreateCacheCluster|2xx->2xx|$.CreateCacheClusterResponse.CreateCacheClusterResult.CacheCluster.ConfigurationEndpoint.Address
Issue-Key: elasticache|CreateCacheCluster|2xx->2xx|$.CreateCacheClusterResponse.CreateCacheClusterResult.CacheCluster.DeleteProtection
Issue-Key: elasticache|CreateCacheCluster|2xx->2xx|$.CreateCacheClusterResponse.CreateCacheClusterResult.CacheCluster.IpDiscovery
Issue-Key: elasticache|CreateCacheCluster|2xx->2xx|$.CreateCacheClusterResponse.CreateCacheClusterResult.CacheCluster.LogDeliveryConfigurations
Issue-Key: elasticache|CreateCacheCluster|2xx->2xx|$.CreateCacheClusterResponse.CreateCacheClusterResult.CacheCluster.NetworkType
Issue-Key: elasticache|CreateCacheCluster|2xx->2xx|$.CreateCacheClusterResponse.CreateCacheClusterResult.CacheCluster.PendingModifiedValues
Issue-Key: elasticache|CreateCacheCluster|2xx->2xx|$.CreateCacheClusterResponse.CreateCacheClusterResult.CacheCluster.ReplicationGroupLogDeliveryEnabled
Issue-Key: elasticache|CreateCacheCluster|2xx->2xx|$.CreateCacheClusterResponse.CreateCacheClusterResult.CacheCluster.TransitEncryptionEnabled
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.AtRestEncryptionEnabled
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.AutoMinorVersionUpgrade
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.CacheParameterGroup.CacheNodeIdsToReboot
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.CacheSecurityGroups
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.ClientDownloadLandingPage
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.ConfigurationEndpoint.Address
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.DeleteProtection
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.IpDiscovery
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.LogDeliveryConfigurations
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.NetworkType
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.PendingModifiedValues
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.ReplicationGroupLogDeliveryEnabled
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.TransitEncryptionEnabled
Issue-Key: elasticache|DeleteCacheCluster|2xx->2xx|$.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.AtRestEncryptionEnabled
Issue-Key: elasticache|DeleteCacheCluster|2xx->2xx|$.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.AutoMinorVersionUpgrade
Issue-Key: elasticache|DeleteCacheCluster|2xx->2xx|$.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.CacheParameterGroup.CacheNodeIdsToReboot
Issue-Key: elasticache|DeleteCacheCluster|2xx->2xx|$.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.CacheSecurityGroups
Issue-Key: elasticache|DeleteCacheCluster|2xx->2xx|$.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.ClientDownloadLandingPage
Issue-Key: elasticache|DeleteCacheCluster|2xx->2xx|$.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.ConfigurationEndpoint.Address
Issue-Key: elasticache|DeleteCacheCluster|2xx->2xx|$.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.DeleteProtection
Issue-Key: elasticache|DeleteCacheCluster|2xx->2xx|$.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.IpDiscovery
Issue-Key: elasticache|DeleteCacheCluster|2xx->2xx|$.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.LogDeliveryConfigurations
Issue-Key: elasticache|DeleteCacheCluster|2xx->2xx|$.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.NetworkType
Issue-Key: elasticache|DeleteCacheCluster|2xx->2xx|$.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.PendingModifiedValues
Issue-Key: elasticache|DeleteCacheCluster|2xx->2xx|$.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.ReplicationGroupLogDeliveryEnabled
Issue-Key: elasticache|DeleteCacheCluster|2xx->2xx|$.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.TransitEncryptionEnabled
Issue-Key: elasticache|CreateReplicationGroup|2xx->2xx|$.CreateReplicationGroupResponse.CreateReplicationGroupResult.ReplicationGroup.AutoMinorVersionUpgrade
Issue-Key: elasticache|CreateReplicationGroup|2xx->2xx|$.CreateReplicationGroupResponse.CreateReplicationGroupResult.ReplicationGroup.DeleteProtection
Issue-Key: elasticache|CreateReplicationGroup|2xx->2xx|$.CreateReplicationGroupResponse.CreateReplicationGroupResult.ReplicationGroup.GlobalReplicationGroupInfo
Issue-Key: elasticache|CreateReplicationGroup|2xx->2xx|$.CreateReplicationGroupResponse.CreateReplicationGroupResult.ReplicationGroup.IpDiscovery
Issue-Key: elasticache|CreateReplicationGroup|2xx->2xx|$.CreateReplicationGroupResponse.CreateReplicationGroupResult.ReplicationGroup.LogDeliveryConfigurations
Issue-Key: elasticache|CreateReplicationGroup|2xx->2xx|$.CreateReplicationGroupResponse.CreateReplicationGroupResult.ReplicationGroup.NetworkType
Issue-Key: elasticache|CreateReplicationGroup|2xx->2xx|$.CreateReplicationGroupResponse.CreateReplicationGroupResult.ReplicationGroup.PendingModifiedValues
Issue-Key: elasticache|DescribeReplicationGroups|2xx->2xx|$.DescribeReplicationGroupsResponse.DescribeReplicationGroupsResult.ReplicationGroups.ReplicationGroup.AutoMinorVersionUpgrade
Issue-Key: elasticache|DescribeReplicationGroups|2xx->2xx|$.DescribeReplicationGroupsResponse.DescribeReplicationGroupsResult.ReplicationGroups.ReplicationGroup.DeleteProtection
Issue-Key: elasticache|DescribeReplicationGroups|2xx->2xx|$.DescribeReplicationGroupsResponse.DescribeReplicationGroupsResult.ReplicationGroups.ReplicationGroup.GlobalReplicationGroupInfo
Issue-Key: elasticache|DescribeReplicationGroups|2xx->2xx|$.DescribeReplicationGroupsResponse.DescribeReplicationGroupsResult.ReplicationGroups.ReplicationGroup.IpDiscovery
Issue-Key: elasticache|DescribeReplicationGroups|2xx->2xx|$.DescribeReplicationGroupsResponse.DescribeReplicationGroupsResult.ReplicationGroups.ReplicationGroup.LogDeliveryConfigurations
Issue-Key: elasticache|DescribeReplicationGroups|2xx->2xx|$.DescribeReplicationGroupsResponse.DescribeReplicationGroupsResult.ReplicationGroups.ReplicationGroup.NetworkType
Issue-Key: elasticache|DescribeReplicationGroups|2xx->2xx|$.DescribeReplicationGroupsResponse.DescribeReplicationGroupsResult.ReplicationGroups.ReplicationGroup.NodeGroups.NodeGroup.NodeGroupMembers.NodeGroupMember.ReadEndpoint.Port
Issue-Key: elasticache|DescribeReplicationGroups|2xx->2xx|$.DescribeReplicationGroupsResponse.DescribeReplicationGroupsResult.ReplicationGroups.ReplicationGroup.NodeGroups.NodeGroup.PrimaryEndpoint.Port
Issue-Key: elasticache|DescribeReplicationGroups|2xx->2xx|$.DescribeReplicationGroupsResponse.DescribeReplicationGroupsResult.ReplicationGroups.ReplicationGroup.NodeGroups.NodeGroup.ReaderEndpoint
Issue-Key: elasticache|DescribeReplicationGroups|2xx->2xx|$.DescribeReplicationGroupsResponse.DescribeReplicationGroupsResult.ReplicationGroups.ReplicationGroup.PendingModifiedValues
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.CacheNodes.CacheNode.Endpoint.Port
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.EngineVersion
Issue-Key: elasticache|DeleteReplicationGroup|2xx->2xx|$.DeleteReplicationGroupResponse.DeleteReplicationGroupResult.ReplicationGroup.AutoMinorVersionUpgrade
Issue-Key: elasticache|DeleteReplicationGroup|2xx->2xx|$.DeleteReplicationGroupResponse.DeleteReplicationGroupResult.ReplicationGroup.DeleteProtection
Issue-Key: elasticache|DeleteReplicationGroup|2xx->2xx|$.DeleteReplicationGroupResponse.DeleteReplicationGroupResult.ReplicationGroup.GlobalReplicationGroupInfo
Issue-Key: elasticache|DeleteReplicationGroup|2xx->2xx|$.DeleteReplicationGroupResponse.DeleteReplicationGroupResult.ReplicationGroup.IpDiscovery
Issue-Key: elasticache|DeleteReplicationGroup|2xx->2xx|$.DeleteReplicationGroupResponse.DeleteReplicationGroupResult.ReplicationGroup.LogDeliveryConfigurations
Issue-Key: elasticache|DeleteReplicationGroup|2xx->2xx|$.DeleteReplicationGroupResponse.DeleteReplicationGroupResult.ReplicationGroup.NetworkType
Issue-Key: elasticache|DeleteReplicationGroup|2xx->2xx|$.DeleteReplicationGroupResponse.DeleteReplicationGroupResult.ReplicationGroup.PendingModifiedValues
Issue-Commit: 5c103ae721112cfc59443e8c0b52708204a7cb08

# ElastiCache cluster and replication-group projections omit always-present modeled members and ignore the requested cache Port

> _Note: this issue was identified against the code base at commit `5c103ae721112cfc59443e8c0b52708204a7cb08`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCacheClusters

## Description
Seven net-new causes, one shared fault: every ElastiCache projection of a CacheCluster or a ReplicationGroup is built from a partial member set, so members AWS always populates are absent from the wire, and the replication-group/cluster endpoint projection does not carry the port the client asked for. All seven are the same serializer/projection defect seen on seven different operations; each must be closed separately because each is exercised by its own fixture step.

Cause 1 of 7: CreateCacheCluster response omits always-present CacheCluster members.
AWS: Smithy com.amazonaws.elasticache#CacheCluster models AutoMinorVersionUpgrade (Boolean), NetworkType (enum NetworkType: ipv4|ipv6|dual_stack), IpDiscovery (enum IpDiscovery: ipv4|ipv6), PendingModifiedValues (PendingModifiedValues), LogDeliveryConfigurations (list), CacheSecurityGroups (list), ClientDownloadLandingPage (String), AtRestEncryptionEnabled / TransitEncryptionEnabled (BooleanOptional), ReplicationGroupLogDeliveryEnabled (Boolean) and CacheParameterGroup (CacheParameterGroupStatus, whose CacheNodeIdsToReboot list is always serialized). The recorded AWS response for a plain cluster create carries all of them: the booleans with their resolved values (AutoMinorVersionUpgrade true; AtRestEncryptionEnabled, TransitEncryptionEnabled, DeleteProtection and ReplicationGroupLogDeliveryEnabled false), NetworkType and IpDiscovery as 'ipv4', the empty lists/structures as empty elements, and ClientDownloadLandingPage as the ElastiCache client-download console URL. The recording also shows NO ConfigurationEndpoint on this response.
Emulator: every one of those members is absent (got None), and it additionally emits a ConfigurationEndpoint.Address that AWS does not return here, advertising a *.cache.amazonaws.com host the emulator does not own and cannot be reached on (replay flagged host_not_own).
Why: an SDK that reads cluster.auto_minor_version_upgrade(), .network_type(), .ip_discovery(), .at_rest_encryption_enabled() or iterates .cache_security_groups()/.log_delivery_configurations() gets null/absent instead of a resolved value, so a client that branches on encryption or network type takes the wrong branch and a client that follows ConfigurationEndpoint connects to a host that is not the emulator.
Fix: the CreateCacheCluster response projection/serializer for CacheCluster — populate the full modeled member set from state defaults, and do not synthesize a ConfigurationEndpoint where AWS returns none (and never advertise a host other than the one the emulator serves).
Traces: elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found#task#16.

Cause 2 of 7: DescribeCacheClusters read-back omits the same CacheCluster members.
AWS: same CacheCluster shape; the recorded read-back carries AutoMinorVersionUpgrade, NetworkType, IpDiscovery, PendingModifiedValues, LogDeliveryConfigurations, CacheSecurityGroups, CacheParameterGroup.CacheNodeIdsToReboot, ClientDownloadLandingPage, AtRestEncryptionEnabled, TransitEncryptionEnabled, DeleteProtection and ReplicationGroupLogDeliveryEnabled.
Emulator: all absent, and ConfigurationEndpoint.Address is a *.cache.amazonaws.com host the emulator does not own.
Why: a client that creates then describes cannot read back the cluster's network/encryption configuration at all.
Fix: the DescribeCacheClusters projection for CacheCluster (same projection as cause 1).
Traces: elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found#task#17.

Cause 3 of 7: DeleteCacheCluster response omits the same CacheCluster members.
AWS: DeleteCacheCluster returns the full CacheCluster projection with CacheClusterStatus 'deleting' and the same always-present members listed in cause 1.
Emulator: all absent; ConfigurationEndpoint.Address again a host it does not own.
Why: the delete acknowledgement is not usable as a cluster snapshot, so a client that keys off the returned projection sees an object with no configuration.
Fix: the DeleteCacheCluster response projection for CacheCluster.
Traces: elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found#task#18.

Cause 4 of 7: CreateReplicationGroup response omits always-present ReplicationGroup members.
AWS: Smithy com.amazonaws.elasticache#ReplicationGroup models AutoMinorVersionUpgrade (Boolean), NetworkType (enum), IpDiscovery (enum), PendingModifiedValues (ReplicationGroupPendingModifiedValues), LogDeliveryConfigurations (list) and GlobalReplicationGroupInfo (GlobalReplicationGroupInfo). The recorded create response carries all of them (AutoMinorVersionUpgrade true, DeleteProtection false, NetworkType/IpDiscovery 'ipv4', the empty structures as empty elements).
Emulator: all absent (got None).
Why: a client cannot tell whether the group is in a Global datastore, what its network type is, or whether a modification is pending.
Fix: the CreateReplicationGroup response projection/serializer for ReplicationGroup.
Traces: elasticache-replication-group-explicit-port-read-back-tags-and-not-found#task#4.

Cause 5 of 7: DescribeReplicationGroups omits the same members, drops NodeGroup.ReaderEndpoint, and reports a port other than the one requested.
AWS: ReplicationGroup as in cause 4; Smithy com.amazonaws.elasticache#NodeGroup models PrimaryEndpoint and ReaderEndpoint (both Endpoint) plus NodeGroupMembers[].ReadEndpoint. When CreateReplicationGroup is given an explicit Port, every Endpoint.Port in the read-back is that requested port, and a replication group with a replica reports a ReaderEndpoint.
Emulator: GlobalReplicationGroupInfo, PendingModifiedValues, LogDeliveryConfigurations, DeleteProtection, NetworkType, IpDiscovery and AutoMinorVersionUpgrade are absent; NodeGroup.ReaderEndpoint is absent entirely; and NodeGroup.PrimaryEndpoint.Port and NodeGroupMembers[].ReadEndpoint.Port carry an emulator-chosen port instead of the Port the create request supplied (replay: expected '6379' — the requested port — got an unrelated high port).
Why: a client that reads the port back from the API to configure its cache client connects to the wrong port and never reaches the cache, and a client that uses the reader endpoint for read scaling has no endpoint to use.
Fix: the DescribeReplicationGroups projection plus the node-group endpoint derivation in replication-group state — the endpoint port must be the stored requested Port, and the reader endpoint must be projected for groups that have replicas.
Traces: elasticache-replication-group-explicit-port-read-back-tags-and-not-found#task#5.

Cause 6 of 7: DescribeCacheClusters for a replication-group member omits the same CacheCluster members and reports the wrong node Endpoint.Port.
AWS: CacheCluster as in cause 1; CacheNodes[].Endpoint.Port is the port the replication group was created with, and ClientDownloadLandingPage is populated.
Emulator: LogDeliveryConfigurations, DeleteProtection, CacheSecurityGroups, CacheParameterGroup.CacheNodeIdsToReboot, NetworkType, IpDiscovery, PendingModifiedValues, AutoMinorVersionUpgrade and ClientDownloadLandingPage are absent, and CacheNodes[].Endpoint.Port is the emulator-chosen port, not the requested one.
Why: same as cause 5 — the per-node endpoint a client dials is wrong.
Fix: the DescribeCacheClusters projection and the cache-node endpoint derivation in cluster state.
Traces: elasticache-replication-group-explicit-port-read-back-tags-and-not-found#task#6.

Cause 7 of 7: DeleteReplicationGroup response omits the same ReplicationGroup members.
AWS: the delete acknowledgement returns the full ReplicationGroup projection with Status 'deleting' and the members listed in cause 4.
Emulator: LogDeliveryConfigurations, DeleteProtection, GlobalReplicationGroupInfo, NetworkType, IpDiscovery, PendingModifiedValues and AutoMinorVersionUpgrade all absent.
Why: the delete acknowledgement is not a usable projection of the group being deleted.
Fix: the DeleteReplicationGroup response projection for ReplicationGroup.
Traces: elasticache-replication-group-explicit-port-read-back-tags-and-not-found#task#14.

Related open issues that do NOT cover these steps: 20261114T012429Z-elasticache-user-group-and-serverless-cache-projections-omit (user-group/serverless-cache projections) and 20261108T134359Z-elasticache-cache-subnet-group-responses-omit-the-modeled (cache subnet group SupportedNetworkTypes). Neither names the CacheCluster/ReplicationGroup projections or these fixture steps, and replay reported no owning issue for any of the seven clusters.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200). All seven clusters: verdict=mismatch, expected_status=200, actual_status=200, service=elasticache, stub_path empty, owned_by empty.

1) elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found#task#16 CreateCacheCluster: $...CacheCluster.LogDeliveryConfigurations missing (expected '', got None); AtRestEncryptionEnabled missing (expected 'false'); DeleteProtection missing (expected 'false'); CacheSecurityGroups missing (expected ''); CacheParameterGroup.CacheNodeIdsToReboot missing (expected ''); NetworkType missing (expected 'ipv4'); ClientDownloadLandingPage missing (expected the ElastiCache client-download console URL); IpDiscovery missing (expected 'ipv4'); PendingModifiedValues missing (expected ''); TransitEncryptionEnabled missing (expected 'false'); AutoMinorVersionUpgrade missing (expected 'true'); ReplicationGroupLogDeliveryEnabled missing (expected 'false'); ConfigurationEndpoint.Address host_not_own (expected None, got a '<cluster>.cfg.<region>.cache.amazonaws.com' host).
2) #task#17 DescribeCacheClusters: identical missing set under DescribeCacheClustersResult.CacheClusters.CacheCluster; ConfigurationEndpoint.Address host_not_own.
3) #task#18 DeleteCacheCluster: identical missing set under DeleteCacheClusterResult.CacheCluster; ConfigurationEndpoint.Address host_not_own.
4) elasticache-replication-group-explicit-port-read-back-tags-and-not-found#task#4 CreateReplicationGroup: ReplicationGroup.LogDeliveryConfigurations, DeleteProtection, GlobalReplicationGroupInfo, NetworkType, IpDiscovery, PendingModifiedValues, AutoMinorVersionUpgrade all missing (expected '', 'false', '', 'ipv4', 'ipv4', '', 'true').
5) #task#5 DescribeReplicationGroups: same missing set, plus NodeGroups.NodeGroup.PrimaryEndpoint.Port value_mismatch (expected '6379' = the requested Port, got '41641'), NodeGroupMembers.NodeGroupMember.ReadEndpoint.Port value_mismatch (expected '6379', got '41641'), NodeGroups.NodeGroup.ReaderEndpoint missing (expected an Endpoint with Address+Port, got None).
6) #task#6 DescribeCacheClusters: CacheCluster.LogDeliveryConfigurations, DeleteProtection, CacheSecurityGroups, CacheParameterGroup.CacheNodeIdsToReboot, NetworkType, IpDiscovery, PendingModifiedValues, AutoMinorVersionUpgrade, ClientDownloadLandingPage missing; CacheNodes.CacheNode.Endpoint.Port value_mismatch (expected '6379', got '41641').
7) #task#14 DeleteReplicationGroup: ReplicationGroup.LogDeliveryConfigurations, DeleteProtection, GlobalReplicationGroupInfo, NetworkType, IpDiscovery, PendingModifiedValues, AutoMinorVersionUpgrade missing.

(The concrete host names, ids and the '41641' port are per-run values; the requirement is that the modeled members are present with resolved values and that the echoed port equals the Port supplied on create.)

## Steps to Reproduce
1. Clone the repo and boot the emulator, then replay the bound wire corpus (replay_wire_corpus).
2. Fixture elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found, steps task#16/task#17/task#18: create a memcached cache cluster in a cache subnet group, describe it, delete it; compare the CacheCluster element against the recorded response.
3. Fixture elasticache-replication-group-explicit-port-read-back-tags-and-not-found, step task#4: CreateReplicationGroup with an explicit Port and one replica; then task#5 DescribeReplicationGroups and task#6 DescribeCacheClusters — check every Endpoint.Port equals the requested Port and that NodeGroup.ReaderEndpoint is present; then task#14 DeleteReplicationGroup.
4. In each response, diff the CacheCluster / ReplicationGroup element against the modeled member set; the members listed in the evidence are absent.
