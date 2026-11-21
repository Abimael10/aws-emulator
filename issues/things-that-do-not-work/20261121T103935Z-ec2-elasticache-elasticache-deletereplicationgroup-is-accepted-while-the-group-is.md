Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__elasticache.vvd-valkey-replication-group-lives-in-the-vpc-and-pins-its-network
Issue-Key: ec2__elasticache.vvd-valkey-replication-group-lives-in-the-vpc-and-pins-its-network|_error#12|AssertionError|raise AssertionError("the call succeeded; real AWS rejects it")
Issue-Commit: 0046bef99fc51ab9ab45bb6f9c8d889fe477827c

# ElastiCache DeleteReplicationGroup is accepted while the group is still creating, instead of refusing InvalidReplicationGroupState naming the member cache cluster

> _Note: this issue was identified against the code base at commit `0046bef99fc51ab9ab45bb6f9c8d889fe477827c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteReplicationGroup

## Description
One net-new root cause, grounded in the single failing ec2__elasticache check the replay report binds to no open issue.

Cause 1 of 1: DeleteReplicationGroup on a replication group that is still `creating` is ACCEPTED instead of refused.
  AWS: Smithy DeleteReplicationGroup declares InvalidReplicationGroupStateFault with awsQueryError code `InvalidReplicationGroupState`, HTTP 400. A CreateReplicationGroup answers Status 'creating' with MemberClusters ['<rg-id>-001'], and while the group is in that window a DeleteReplicationGroup is refused with `Cache cluster <member-cache-cluster-id> is not in a valid state to be deleted.` — the sentence is fixed apart from the member cache cluster id, which comes from the group's own MemberClusters in service state (the primary's '-001' member for a one-node group). The delete becomes possible only once the group reaches 'available'.
  Emulator: the delete returns successfully — no ClientError is raised — so a one-node Valkey group created a moment earlier can be torn down inside the window AWS protects.
  Why: a client that creates a replication group and expects the id to be immutable until it settles (the ordering AWS enforces, and the reason provisioning code waits before issuing teardown) sees the opposite: the delete lands, the ENI/subnet dependencies it was about to assert on disappear, and its handling of InvalidReplicationGroupState is never exercised. Any emulator-backed test of create/delete sequencing passes while the same code fails against AWS.
  Fix: DeleteReplicationGroup's handler state gate must consult the group's Status and its member cache clusters' states before mutating anything, and emit the InvalidReplicationGroupState refusal naming the offending member cluster; the group's state machine must also keep a non-available window after create for the gate to observe.
  Note: open issue 20261114T131431Z-elasticache-transient-lifecycle-windows-are-not-observable-creating.md reports the creating/modifying/deleting windows settling too fast for other elasticache checks; it does not carry this check or this operation's refusal, so it does not cover this one.
  Traces: ec2__elasticache.vvd-valkey-replication-group-lives-in-the-vpc-and-pins-its-network (check line 39, via the _error helper at line 182; the duplicate-create leg immediately above it passes); handler services/service-com-amazonaws-elasticache-amazonelasticachev9/src/ops/delete_replication_group.rs.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: ec2__elasticache 14/15 (93%), 1 failure — the one below.

[mismatch] ec2__elasticache.vvd-valkey-replication-group-lives-in-the-vpc-and-pins-its-network
check_ec2__elasticache_vvd_valkey_replication_group_lives_in_the_vpc_and_pins_its_network.py:39 `got = _error(lambda: ec.delete_replication_group(ReplicationGroupId=rg))` -> :182
AssertionError: the call succeeded; real AWS rejects it

The check's preceding step confirms the group was still in its creating window: CreateReplicationGroup answered Status 'creating' with MemberClusters ['<rg>-001'], and the duplicate CreateReplicationGroup was correctly refused with ('ReplicationGroupAlreadyExists', 'Replication group with specified name already exists.').

Expected (AWS): HTTP 400, Error.Code 'InvalidReplicationGroupState', Error.Message 'Cache cluster <rg>-001 is not in a valid state to be deleted.' — fixed text except the member cache cluster id taken from the group's MemberClusters.
Actual (emulator): the DeleteReplicationGroup call returns a successful response, so no code or message is produced at all.

The replication-group id, the member cluster suffix value, the VPC/subnet ids and the run's generated names are per-run values and are not part of the requirement; the HTTP status, the error code and the message template are.

## Steps to Reproduce
1. Create a VPC with two /24 subnets in two availability zones and an ElastiCache cache subnet group over them.
2. CreateReplicationGroup(ReplicationGroupId=<rg>, ReplicationGroupDescription=<text>, Engine='valkey', EngineVersion='8.0', CacheNodeType='cache.t3.micro', NumCacheClusters=1, CacheSubnetGroupName=<group>, TransitEncryptionEnabled=True). The response reports Status='creating' and MemberClusters=['<rg>-001'].
3. Without waiting, call DeleteReplicationGroup(ReplicationGroupId=<rg>).
4. Expect HTTP 400 with Error.Code 'InvalidReplicationGroupState' and Error.Message 'Cache cluster <rg>-001 is not in a valid state to be deleted.', and DescribeReplicationGroups still reporting the group as creating. Observe instead a successful DeleteReplicationGroup response.

Re-replay ec2__elasticache.vvd-valkey-replication-group-lives-in-the-vpc-and-pins-its-network to confirm the fix.
