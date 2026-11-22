Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__ec2__elasticache.e2e-vvd-create-vpc-valkey-dynamodb-lifecycle
Issue-Key: dynamodb__ec2__elasticache.e2e-vvd-create-vpc-valkey-dynamodb-lifecycle|phase#9|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Commit: 1b54d60a21bdcb9d9cbbbe8d2d80ba69001d0d46

# A cache subnet group that outlives its deleted subnets keeps listing them, so the orphaned-group read in the VPC/Valkey/DynamoDB lifecycle never matches AWS

> _Note: this issue was identified against the code base at commit `1b54d60a21bdcb9d9cbbbe8d2d80ba69001d0d46`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCacheSubnetGroups

## Description
One net-new root cause, grounded in the one failing dynamodb__ec2__elasticache check this run's replay report binds to no open issue.

Cause 1 of 1: after the subnets and the VPC under a cache subnet group are deleted, DescribeCacheSubnetGroups still projects the group's original Subnets list instead of re-deriving it from live EC2 state.
AWS: recorded behavior only (no Smithy trait governs cross-service derivation; CacheSubnetGroup carries VpcId and Subnets[].SubnetIdentifier/SubnetAvailabilityZone). The scenario's restore phase deletes the replication group, then the subnets, then the VPC, while the cache subnet group itself is left behind — EC2 permits that, so the group OUTLIVES its network. The recorded read of that orphan still reports the (now deleted) VpcId it was created in, and an EMPTY Subnets list, because a subnet that no longer exists cannot be a member: the expectation is exactly `[(VpcId, Subnets)] == [(<the deleted VPC's id>, [])]`.
Emulator: the group reads back with its full original membership — VpcId plus both SubnetIdentifiers (each with its SubnetAvailabilityZone and SupportedNetworkTypes) — even though DescribeSubnets/DescribeVpcs no longer resolve any of them (the check has already confirmed the VPC answers InvalidVpcID.NotFound). The subnet/VPC ids in the evidence are per-run values; the requirement is that the projection lists only subnets that still exist.
Why: a client that inspects an orphaned subnet group to decide what it may still reuse or must clean up is told it still spans two live subnets in a live VPC. Any reconciliation that trusts Subnets (re-creating a cluster in the group, or deciding the group is safe to keep) acts on a network that no longer exists, and the operator-visible signal AWS gives for 'this group has no usable subnets left' never appears.
Fix: the CacheSubnetGroup projection read by DescribeCacheSubnetGroups (and by the cache-subnet-group state it is built from) must re-derive its Subnets/SupportedNetworkTypes from the current EC2 subnet state at read time, rather than returning the membership captured at create time. Related open issue 20261110T144301Z-ec2-elasticache-elasticache-cache-subnet-group-never-re-derives-subnets.md reports the same mechanism under the `ec2__elasticache` service heading but carries a different replay key and not this check id, so this failure is bound to no issue. The same scenario's earlier cause (the lifecycle timing out before any assertion) is filed as 20261108T092824Z-dynamodb-ec2-elasticache-2-net-new-causes-deletes.md; the flow now gets much further and fails on this read instead.
Traces: dynamodb__ec2__elasticache.e2e-vvd-create-vpc-valkey-dynamodb-lifecycle.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: service heading `### dynamodb__ec2__elasticache` 0/2 passing; the sibling check is OWNED by an existing issue, this one is bound to none.

dynamodb__ec2__elasticache.e2e-vvd-create-vpc-valkey-dynamodb-lifecycle (verdict: mismatch), phase 3/5 post_invoke, check file line ~2016:
  assertion: [(o['VpcId'], o['Subnets']) for o in orphan] == [(answer['VpcId'], [])]
  expected: the group still names the deleted VPC and has NO subnets.
  actual:   AssertionError: [phase 3/5 post_invoke] the orphaned subnet group reads [{'CacheSubnetGroupName': 'sim2real-vvd-e2e-11173ee293f9', 'CacheSubnetGroupDescription': 'storage app subnet group', 'VpcId': 'vpc-0000000000000009d', 'Subnets': [{'SubnetIdentifier': 'subnet-000000000000000a5', 'SubnetAvailabilityZone': {'Name': 'us-west-2a'}, 'SupportedNetworkTypes': ['ipv4']}, {'SubnetIdentifier': 'subnet-000000000000000a7', 'SubnetAvailabilityZone': {'Name': 'us-west-2b'}, 'SupportedNetworkTypes': ['ipv4']}], 'ARN': 'arn:aws:elasticache:us-west-2:000000000000:subnetgroup:sim2real-vvd-e2e-11173ee293f9', 'SupportedNetworkTypes': ['ipv4']}]; expected the deleted VPC and no subnets
  Everything before it in the phase passed, including: the replication group stopped resolving with ReplicationGroupNotFoundFault / 'ReplicationGroup <id> not found.', the DynamoDB table stopped resolving with ResourceNotFoundException, and ec2 DescribeVpcs on the VPC answered InvalidVpcID.NotFound — so the subnets and VPC really are gone while the subnet group still lists them. The group name, ids and AZs are per-run values.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. ec2 CreateVpc (10.0.0.0/16) and CreateSubnet in two distinct AZs.
3. elasticache CreateCacheSubnetGroup(CacheSubnetGroupName=<name>, SubnetIds=[both subnets]).
4. Delete everything under the group but NOT the group itself: delete any cache resource using it, then ec2 DeleteSubnet for both subnets and ec2 DeleteVpc. Confirm ec2 DescribeVpcs on that VPC answers InvalidVpcID.NotFound.
5. elasticache DescribeCacheSubnetGroups(CacheSubnetGroupName=<name>). Expect the group to still exist, still report the deleted VpcId, and report an EMPTY Subnets list; observe both deleted SubnetIdentifiers still listed.
6. Re-replay dynamodb__ec2__elasticache.e2e-vvd-create-vpc-valkey-dynamodb-lifecycle; it must get past phase 3/5 post_invoke.
