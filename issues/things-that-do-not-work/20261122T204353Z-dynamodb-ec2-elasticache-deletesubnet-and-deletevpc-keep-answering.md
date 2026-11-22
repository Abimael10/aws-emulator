Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__ec2__elasticache.e2e-vvd-create-vpc-valkey-dynamodb-lifecycle
Issue-Key: dynamodb__ec2__elasticache.e2e-vvd-create-vpc-valkey-dynamodb-lifecycle|phase#9|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Commit: 02c6380a70f664dba771786c55045481408bc4cc

# dynamodb__ec2__elasticache: DeleteSubnet and DeleteVpc keep answering DependencyViolation after the cache resources are gone, so the VPC/Valkey/DynamoDB restore never converges

> _Note: this issue was identified against the code base at commit `02c6380a70f664dba771786c55045481408bc4cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteSubnet

## Description
One net-new root cause, grounded in the one failing dynamodb__ec2__elasticache check this run's replay report binds to no issue. The scenario now gets further than it did when 20261108T092824Z-dynamodb-ec2-elasticache-2-net-new-causes-deletes.md (lifecycle never converged) and 20261116T010037Z-dynamodb-ec2-elasticache-a-cache-subnet-group-that-outlives-its-deleted.md (orphaned subnet-group read) were filed — both fixed; it now fails EARLIER, in the restore's own convergence loop, on a different operation.

Cause 1 of 1: the subnets and the VPC of a torn-down estate can never be deleted — DeleteSubnet and DeleteVpc answer DependencyViolation on every restore pass, even after the replication group that used them is confirmed gone.
AWS: recorded behavior only (EC2 carries its refusals on the ec2Query envelope; DeleteSubnet/DeleteVpc declare errors: [] in Smithy). The scenario's post_invoke restore runs in a fixed order — DeleteReplicationGroup(RetainPrimaryCluster=False) and wait until DescribeReplicationGroups answers not-found, DeleteTable, then for the VPC: DeleteSubnet for every subnet, detach+delete the internet gateway, delete every non-main route table, delete every non-default security group, DeleteVpc. On AWS that order is sufficient and the FIRST pass is clean: once the cache cluster is gone its ElastiCache ENI is released, so each subnet deletes and the VPC follows. A cache subnet group still naming those subnets does NOT hold them: AWS lets the group outlive its deleted subnets and VPC (that is exactly the orphaned-group state the same scenario asserts later, and the restore here never deletes the group — it cannot resolve its name from the replication-group record, so the group is still present on every pass).
Emulator: the restore never converges. Across all three passes DeleteSubnet answers `DependencyViolation` for every subnet in the VPC and DeleteVpc then answers `DependencyViolation` too; between passes the check waits until DescribeReplicationGroups reports the group gone, so the cache cluster is already removed when the deletes are retried. Pass 1: ['delete_subnet: DependencyViolation', 'delete_vpc: DependencyViolation']; passes 2 and 3 add 'delete_table: ResourceInUseException' because the re-run issues a second DeleteTable against the table the first pass already deleted — that extra error only exists because cause 1 forces the re-runs (AWS also answers ResourceInUseException to a DeleteTable on a table still DELETING, so it is collateral, not an independent divergence; if the emulator's DELETING window never ends it must settle too). Hypothesis: the subnet's delete gate counts a dependent that AWS does not — the surviving cache subnet group that still lists the subnet, or an ElastiCache ENI that is never released when the replication group is deleted.
Why: a client cannot tear down its own network. Every estate built on an ElastiCache subnet group leaks its subnets and VPC forever, each retry costing another DependencyViolation with nothing to act on, and the account hits its VPC/subnet quotas; no reconciliation or cleanup loop can ever finish.
Fix: the EC2 DeleteSubnet and DeleteVpc dependency checks and the ElastiCache replication-group delete path they read — a deleted replication group must release the network interfaces it held in the subnet, and a cache subnet group that merely names a subnet must not block that subnet's (or its VPC's) deletion.
Traces: dynamodb__ec2__elasticache.e2e-vvd-create-vpc-valkey-dynamodb-lifecycle (phase 3/5 post_invoke).

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout; service heading `### dynamodb__ec2__elasticache` (1/2 passing — the sibling check is OWNED by an existing issue, this one is bound to none).

dynamodb__ec2__elasticache.e2e-vvd-create-vpc-valkey-dynamodb-lifecycle (verdict: mismatch), check file line 461:
  assertion: passes[-1]['errors'] == []  (the restore must converge within 3 passes, and a further restore over a restored state must report nothing)
  actual:   AssertionError: [phase 3/5 post_invoke] post_invoke did not converge in 3 passes:
            [{'subnet_group': '', 'errors': ['delete_subnet: DependencyViolation', 'delete_vpc: DependencyViolation']},
             {'subnet_group': '', 'errors': ['delete_table: ResourceInUseException', 'delete_subnet: DependencyViolation', 'delete_vpc: DependencyViolation']},
             {'subnet_group': '', 'errors': ['delete_table: ResourceInUseException', 'delete_subnet: DependencyViolation', 'delete_vpc: DependencyViolation']}]
  Restore order in the check (lines 62155-62219 of the replay listing): DeleteReplicationGroup + wait for gone -> DeleteTable -> DeleteSubnet per subnet -> detach/delete IGW -> delete non-main route tables -> delete non-default security groups -> DeleteVpc; errors are collected, never raised. Between passes the check polls until DescribeReplicationGroups reports the group GONE, so the cache cluster is removed before each retry. 'subnet_group': '' on every pass shows the cache subnet group is never deleted by the restore and is therefore present throughout. The IGW, route-table and security-group deletes all succeed (no entries for them), so the subnets are the only blocked dependents.

Status/code requirements quoted: DeleteSubnet and DeleteVpc must answer 200 here; the diverging refusal is HTTP 400 with Error.Code `DependencyViolation`. The subnet/VPC ids in a run are per-run values.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. ec2 CreateVpc(10.0.0.0/16) + CreateSubnet in two distinct AZs; ec2 CreateInternetGateway + AttachInternetGateway.
3. elasticache CreateCacheSubnetGroup(CacheSubnetGroupName=<group>, SubnetIds=[both subnets]).
4. elasticache CreateReplicationGroup(Engine='valkey', EngineVersion='8.0', CacheNodeType='cache.t3.micro', NumCacheClusters=1, CacheSubnetGroupName=<group>, TransitEncryptionEnabled=True); wait for 'available'. Create any DynamoDB table alongside it.
5. elasticache DeleteReplicationGroup(ReplicationGroupId=<id>, RetainPrimaryCluster=False) and poll DescribeReplicationGroups until it answers not-found. Do NOT delete the cache subnet group.
6. dynamodb DeleteTable; then ec2 DeleteSubnet for both subnets, detach+delete the IGW, delete non-main route tables and non-default security groups, then ec2 DeleteVpc. Each delete must succeed; observe DeleteSubnet and DeleteVpc answering 400 DependencyViolation on this and on every retry.
7. Re-replay dynamodb__ec2__elasticache.e2e-vvd-create-vpc-valkey-dynamodb-lifecycle; phase 3/5 post_invoke must converge on its first pass.
