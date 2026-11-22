Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__elasticache.vvd-valkey-replication-group-lives-in-the-vpc-and-pins-its-network
Issue-Key: ec2__elasticache.vvd-valkey-replication-group-lives-in-the-vpc-and-pins-its-network|run#20|AssertionError|assert got == want, f"the available group reads {got!r}, expected {want!r}"
Issue-Commit: 02c6380a70f664dba771786c55045481408bc4cc

# ec2__elasticache: an available Valkey replication group omits TransitEncryptionMode and NetworkType, so DescribeReplicationGroups never reports the defaults AWS resolves

> _Note: this issue was identified against the code base at commit `02c6380a70f664dba771786c55045481408bc4cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeReplicationGroups

## Description
One net-new root cause, grounded in the one failing ec2__elasticache check this run's replay report binds to no issue.

Cause 1 of 1: a replication group created with TransitEncryptionEnabled=true and no explicit TransitEncryptionMode/NetworkType settles `available` with both members absent from its projection, instead of the values AWS resolves for it.
AWS: Smithy (services/service-com-amazonaws-elasticache-amazonelasticachev9) declares ReplicationGroup.TransitEncryptionMode -> TransitEncryptionMode ("A setting that allows you to migrate your clients to use in-transit encryption, with no downtime") and ReplicationGroup.NetworkType -> NetworkType ("Must be either ipv4 | ipv6 | dual_stack"); neither is request-only, both are part of the resource's state. The recording: CreateReplicationGroup(Engine='valkey', EngineVersion='8.0', CacheNodeType='cache.t3.micro', NumCacheClusters=1, CacheSubnetGroupName=<group>, TransitEncryptionEnabled=True) — no TransitEncryptionMode, no NetworkType — settles `available` and DescribeReplicationGroups then reports TransitEncryptionMode `required` and NetworkType `ipv4`, i.e. the service resolves the omitted settings to their defaults and publishes them. Every other member of the same read matches already (Engine valkey, CacheNodeType cache.t3.micro, ClusterEnabled false, ClusterMode disabled, AutomaticFailover disabled, MultiAZ disabled, TransitEncryptionEnabled true, AtRestEncryptionEnabled true, AuthTokenEnabled false, one member cluster).
Emulator: both members are absent from the projection (botocore reads them as None) while every sibling member is correct, so the group reports in-transit encryption enabled but declines to say in which mode, and reports no network type at all.
Why: TransitEncryptionMode is the field a client reads to know whether its clients may still connect in clear text (`preferred`) or must all use TLS (`required`) — a migration cannot be driven without it, and a caller that reads None concludes the setting is unavailable. NetworkType is what an operator checks to confirm the group is reachable over IPv4 before wiring an application to it; absent, any IPv4/IPv6/dual-stack decision is made blind.
Fix: the ElastiCache replication-group state and the projection DescribeReplicationGroups/CreateReplicationGroup serialize from — resolve the omitted TransitEncryptionMode (to `required` when transit encryption is enabled at create) and NetworkType (to `ipv4`) at create time and always emit both for a settled group. Related but NOT covering this failure: the open issue 20261118T043247Z-elasticache-cluster-and-replication-group-projections-omit-always.md reports the same class of omission under the `elasticache` heading, and 20261110T144301Z-ec2-elasticache-elasticache-cache-subnet-group-never-re-derives-subnets.md is the other open ec2__elasticache issue; neither carries this check id's replay key.
Traces: ec2__elasticache.vvd-valkey-replication-group-lives-in-the-vpc-and-pins-its-network.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout; service heading `### ec2__elasticache` (14/15 passing, this is the only failure and it is marked neither OWNED nor ALREADY FILED).

ec2__elasticache.vvd-valkey-replication-group-lives-in-the-vpc-and-pins-its-network (verdict: mismatch), check file line 56:
  assertion: the (Engine, CacheNodeType, ClusterEnabled, ClusterMode, AutomaticFailover, MultiAZ, TransitEncryptionEnabled, TransitEncryptionMode, AtRestEncryptionEnabled, AuthTokenEnabled, NetworkType, MemberClusters) projection of the available group.
  expected: {... 'TransitEncryptionEnabled': True, 'TransitEncryptionMode': 'required', 'AtRestEncryptionEnabled': True, 'AuthTokenEnabled': False, 'NetworkType': 'ipv4', ...}
  actual:   AssertionError: the available group reads {... 'TransitEncryptionEnabled': True, 'TransitEncryptionMode': None, 'AtRestEncryptionEnabled': True, 'AuthTokenEnabled': False, 'NetworkType': None, ...}
  Everything before it passed: CreateReplicationGroup answered status 'creating' with one member cluster, the duplicate-create refusal and the delete-while-creating refusal both matched, and the group then settled 'available'. The member-cluster name in the output is a per-run value; the requirement is that the two members are present with their resolved defaults.

Smithy cited: com.amazonaws.elasticache#ReplicationGroup members TransitEncryptionMode (enum TransitEncryptionMode) and NetworkType (enum NetworkType: ipv4 | ipv6 | dual_stack).

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. ec2 CreateVpc + CreateSubnet in two distinct AZs; elasticache CreateCacheSubnetGroup over those subnets.
3. elasticache CreateReplicationGroup(ReplicationGroupId=<id>, ReplicationGroupDescription=..., Engine='valkey', EngineVersion='8.0', CacheNodeType='cache.t3.micro', NumCacheClusters=1, CacheSubnetGroupName=<group>, TransitEncryptionEnabled=True) — do NOT pass TransitEncryptionMode or NetworkType.
4. Poll DescribeReplicationGroups(ReplicationGroupId=<id>) until Status leaves 'creating' and reads 'available'.
5. Read the group: TransitEncryptionMode must be 'required' and NetworkType must be 'ipv4'. Observe both absent.
6. Re-replay ec2__elasticache.vvd-valkey-replication-group-lives-in-the-vpc-and-pins-its-network.
