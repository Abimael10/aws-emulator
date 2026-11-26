Issue-Source: trace-sweep-divergence
Issue-Trace: elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found
Issue-Step: task#16
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found elasticache-replication-group-explicit-port-read-back-tags-and-not-found
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache|CreateCacheCluster|2xx->2xx|$.CreateCacheClusterResponse.CreateCacheClusterResult.CacheCluster.DeleteProtection
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.ConfigurationEndpoint.Address
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.DeleteProtection
Issue-Key: elasticache|DeleteCacheCluster|2xx->2xx|$.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.ConfigurationEndpoint.Address
Issue-Key: elasticache|DeleteCacheCluster|2xx->2xx|$.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.DeleteProtection
Issue-Key: elasticache|CreateReplicationGroup|2xx->2xx|$.CreateReplicationGroupResponse.CreateReplicationGroupResult.ReplicationGroup.DeleteProtection
Issue-Key: elasticache|DescribeReplicationGroups|2xx->2xx|$.DescribeReplicationGroupsResponse.DescribeReplicationGroupsResult.ReplicationGroups.ReplicationGroup.DeleteProtection
Issue-Key: elasticache|DescribeReplicationGroups|2xx->2xx|$.DescribeReplicationGroupsResponse.DescribeReplicationGroupsResult.ReplicationGroups.ReplicationGroup.NodeGroups.NodeGroup.NodeGroupMembers.NodeGroupMember.ReadEndpoint.Port
Issue-Key: elasticache|DescribeReplicationGroups|2xx->2xx|$.DescribeReplicationGroupsResponse.DescribeReplicationGroupsResult.ReplicationGroups.ReplicationGroup.NodeGroups.NodeGroup.PrimaryEndpoint.Port
Issue-Key: elasticache|DescribeReplicationGroups|2xx->2xx|$.DescribeReplicationGroupsResponse.DescribeReplicationGroupsResult.ReplicationGroups.ReplicationGroup.NodeGroups.NodeGroup.ReaderEndpoint.Port
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.CacheNodes.CacheNode.Endpoint.Port
Issue-Key: elasticache|DescribeCacheClusters|2xx->2xx|$.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.ClientDownloadLandingPage
Issue-Key: elasticache|DeleteReplicationGroup|2xx->2xx|$.DeleteReplicationGroupResponse.DeleteReplicationGroupResult.ReplicationGroup.DeleteProtection
Issue-Commit: 01f237389f2ba129bcfe6809f1b810fd15e9edea

# ElastiCache cluster and replication-group reads omit DeleteProtection and publish endpoints the emulator does not serve

> _Note: this issue was identified against the code base at commit `01f237389f2ba129bcfe6809f1b810fd15e9edea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCacheCluster

## Description
Four distinct causes in how the ElastiCache emulator shapes the CacheCluster / ReplicationGroup / Endpoint structures it serves on the create, describe and delete paths. All four are 200-vs-200 body divergences: the status and the error surface are right, the described resource is not. THIS BUNDLE CARRIES 4 CAUSES (Cause 1 of 4 .. Cause 4 of 4); all four must be closed before submitting.

Cause 1 of 4 - DeleteProtection is never serialized on a cluster or a replication group (fixtures: elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found#task#16 CreateCacheCluster, #task#17 DescribeCacheClusters, #task#18 DeleteCacheCluster; elasticache-replication-group-explicit-port-read-back-tags-and-not-found#task#4 CreateReplicationGroup, #task#5 DescribeReplicationGroups, #task#14 DeleteReplicationGroup).
AWS: recorded behavior only - the checked-in Smithy model (com.amazonaws.elasticache#CacheCluster, com.amazonaws.elasticache#ReplicationGroup) has no DeleteProtection member, so the model states nothing about it; the recorded AWS responses for every one of those six operations carry the member with its default, i.e. the CacheCluster element contains <DeleteProtection>false</DeleteProtection> and the ReplicationGroup element contains <DeleteProtection>false</DeleteProtection>. The member is unconditional on these paths: it is present whenever the resource is described, created or deleted, and its value is the resource's own delete-protection flag (false unless the caller enabled it).
Emulator: the member is absent from every one of those six response bodies (replay verdict 'missing (expected \'false\', got None)').
Why: a client that reads DeleteProtection to decide whether a cluster/replication group can be torn down (or that round-trips a describe into a modify) sees an unset/absent field where AWS always reports a concrete boolean; SDK-level accessors return null and delete-protection-aware tooling cannot tell a protected resource from an unprotected one.
Fix: the elasticache CacheCluster and ReplicationGroup state records need a delete-protection flag defaulted to false at create, and the CreateCacheCluster / DescribeCacheClusters / DeleteCacheCluster / CreateReplicationGroup / DescribeReplicationGroups / DeleteReplicationGroup serializers must emit it on every element.
Traces: elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found#task#16, #task#17, #task#18; elasticache-replication-group-explicit-port-read-back-tags-and-not-found#task#4, #task#5, #task#14.

Cause 2 of 4 - published cache endpoint ports are the emulator's per-run listener port instead of the cache engine's port (fixtures: elasticache-replication-group-explicit-port-read-back-tags-and-not-found#task#5 DescribeReplicationGroups, #task#6 DescribeCacheClusters).
AWS: Smithy com.amazonaws.elasticache#Endpoint.Port - 'The port number that the cache engine is listening on.' For a Valkey/Redis OSS replication group the recording shows that port reported identically on NodeGroup.PrimaryEndpoint.Port, NodeGroup.ReaderEndpoint.Port, NodeGroupMember.ReadEndpoint.Port and CacheNode.Endpoint.Port, and it is the port the cluster was configured with (the engine's port - 6379 for redis/valkey - in this recording), not an ephemeral value. The requirement is that every endpoint of one cluster reports the configured cache port; the concrete number comes from the request/cluster configuration.
Emulator: all four of those members report the per-run listener port the emulator happens to have bound ('value_mismatch (expected \'6379\', got \'35447\')'), a value that changes on every boot.
Why: a client reads Endpoint.Port to connect, or asserts the port it configured was honoured; it gets a number that is neither the port it asked for nor stable across runs, so configuration read-back and any port assertion fail. (Related but distinct from the open issue 20261123T180239Z-elasticache-serverless-cache-publishes-per-run-listener-ports, which covers serverless caches' Endpoint/ReaderEndpoint; this one is the cluster/replication-group path and is not covered by it.)
Fix: endpoint synthesis in the elasticache state/serializer layer must carry the cluster's configured cache port into every Endpoint it publishes, keeping the internal listener port out of the response.
Traces: elasticache-replication-group-explicit-port-read-back-tags-and-not-found#task#5, #task#6.

Cause 3 of 4 - ConfigurationEndpoint.Address is a real *.cache.amazonaws.com hostname the emulator does not serve (fixtures: elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found#task#17 DescribeCacheClusters, #task#18 DeleteCacheCluster).
AWS: Smithy com.amazonaws.elasticache#CacheCluster.ConfigurationEndpoint -> Endpoint, documented as 'a Memcached cluster endpoint which can be used by an application to connect to any node in the cluster' whose address 'will always have .cfg in it'. The recording does not assert the literal hostname ('<shaped-not-asserted>'): the requirement is that the address be a connectable configuration endpoint for the cluster being described.
Emulator: it announces a host it does not own and cannot answer on - got 'sim2real-probe-ec-mc-<id>.<nodeid>.cfg.<region>.cache.amazonaws.com' (replay verdict host_not_own).
Why: a client that connects to the advertised configuration endpoint is pointed at a name outside the emulator (real AWS DNS or nothing at all), so the memcached discovery path cannot work against the emulator even though the control plane reports the cluster as available.
Fix: the elasticache endpoint/address synthesis must emit the emulator's own reachable host for ConfigurationEndpoint, keeping the .cfg shape, rather than a literal amazonaws.com name.
Traces: elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found#task#17, #task#18.

Cause 4 of 4 - ClientDownloadLandingPage is a hard-coded real AWS console URL (fixture: elasticache-replication-group-explicit-port-read-back-tags-and-not-found#task#6 DescribeCacheClusters).
AWS: Smithy com.amazonaws.elasticache#CacheCluster.ClientDownloadLandingPage - 'The URL of the web page where you can download the latest ElastiCache client library.' The recording does not assert the literal URL ('<shaped-not-asserted>'); the requirement is a URL the serving endpoint owns.
Emulator: got 'https://console.aws.amazon.com/elasticache/home#client-download:' (replay verdict host_not_own) - an address other than the one it is reached on.
Why: anything that follows the advertised URL leaves the emulator entirely, so a client exercising the field talks to a host the emulator does not control.
Fix: the DescribeCacheClusters serializer must render ClientDownloadLandingPage against the emulator's own host instead of a constant console URL.
Traces: elasticache-replication-group-explicit-port-read-back-tags-and-not-found#task#6.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200): 7 divergence clusters, all status 200 expected and 200 actual, body-only mismatches.

Cause 1 (DeleteProtection, verdict mismatch, 6 clusters):
- $.CreateCacheClusterResponse.CreateCacheClusterResult.CacheCluster.DeleteProtection: missing (expected 'false', got None) [elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found#task#16]
- $.DescribeCacheClustersResponse.DescribeCacheClustersResult.CacheClusters.CacheCluster.DeleteProtection: missing (expected 'false', got None) [#task#17]
- $.DeleteCacheClusterResponse.DeleteCacheClusterResult.CacheCluster.DeleteProtection: missing (expected 'false', got None) [#task#18]
- $.CreateReplicationGroupResponse.CreateReplicationGroupResult.ReplicationGroup.DeleteProtection: missing (expected 'false', got None) [elasticache-replication-group-explicit-port-read-back-tags-and-not-found#task#4]
- $.DescribeReplicationGroupsResponse.DescribeReplicationGroupsResult.ReplicationGroups.ReplicationGroup.DeleteProtection: missing (expected 'false', got None) [#task#5]
- $.DeleteReplicationGroupResponse.DeleteReplicationGroupResult.ReplicationGroup.DeleteProtection: missing (expected 'false', got None) [#task#14]

Cause 2 (endpoint port, verdict mismatch, 2 clusters):
- $.DescribeReplicationGroupsResponse...ReplicationGroup.NodeGroups.NodeGroup.PrimaryEndpoint.Port: value_mismatch (expected '6379', got '35447')
- ...NodeGroup.ReaderEndpoint.Port: value_mismatch (expected '6379', got '35447')
- ...NodeGroup.NodeGroupMembers.NodeGroupMember.ReadEndpoint.Port: value_mismatch (expected '6379', got '35447') [elasticache-replication-group-explicit-port-read-back-tags-and-not-found#task#5]
- $.DescribeCacheClustersResponse...CacheCluster.CacheNodes.CacheNode.Endpoint.Port: value_mismatch (expected '6379', got '35447') [#task#6]
(35447 is the emulator's per-run data listener; the replay summary reports the data listener on :39129 for this boot, i.e. the published number is not stable.)

Cause 3 (configuration endpoint host, verdict mismatch / host_not_own, 2 clusters):
- $.DescribeCacheClustersResponse...CacheCluster.ConfigurationEndpoint.Address: host_not_own (expected '<shaped-not-asserted>', got 'sim2real-probe-ec-mc-3c77c8b3.000001.cfg.us-west-2.cache.amazonaws.com') [elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found#task#17]
- $.DeleteCacheClusterResponse...CacheCluster.ConfigurationEndpoint.Address: host_not_own (same value) [#task#18]

Cause 4 (client download URL, verdict mismatch / host_not_own, 1 cluster):
- $.DescribeCacheClustersResponse...CacheCluster.ClientDownloadLandingPage: host_not_own (expected '<shaped-not-asserted>', got 'https://console.aws.amazon.com/elasticache/home#client-download:') [elasticache-replication-group-explicit-port-read-back-tags-and-not-found#task#6]

Model evidence: Smithy com.amazonaws.elasticache#Endpoint.Port = 'The port number that the cache engine is listening on.'; CacheCluster.ClientDownloadLandingPage = 'The URL of the web page where you can download the latest ElastiCache client library.'; CacheCluster/ReplicationGroup in the checked-in model declare no DeleteProtection member (recorded behavior only).

## Steps to Reproduce
1. git clone the project repo and build/boot the emulator from that checkout.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Cause 1: replay fixture elasticache-cache-subnet-group-mixed-case-name-lowercased-tags-second-zone-and-not-found steps task#16 (CreateCacheCluster), task#17 (DescribeCacheClusters), task#18 (DeleteCacheCluster) and fixture elasticache-replication-group-explicit-port-read-back-tags-and-not-found steps task#4 (CreateReplicationGroup), task#5 (DescribeReplicationGroups), task#14 (DeleteReplicationGroup); inspect the CacheCluster/ReplicationGroup element for a DeleteProtection child - it is absent, AWS returns false.
4. Cause 2: in elasticache-replication-group-explicit-port-read-back-tags-and-not-found, create the replication group and then run task#5 / task#6; compare NodeGroup.PrimaryEndpoint.Port, NodeGroup.ReaderEndpoint.Port, NodeGroupMember.ReadEndpoint.Port and CacheNode.Endpoint.Port against the configured cache port (6379) - they report the emulator's listener port, and the value changes between boots.
5. Cause 3: run task#17 / task#18 of the cache-subnet-group fixture and resolve the returned ConfigurationEndpoint.Address - it is a *.cfg.<region>.cache.amazonaws.com name the emulator does not answer on.
6. Cause 4: run task#6 of the replication-group fixture and read ClientDownloadLandingPage - it is the literal real AWS console URL.
