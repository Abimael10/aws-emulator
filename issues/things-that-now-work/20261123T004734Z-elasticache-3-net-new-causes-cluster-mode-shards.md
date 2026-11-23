Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticache.elasticache-cluster-mode-slot-routing-moved-and-crossslot elasticache.global-replication-group-topology-lifecycle elasticache.transit-encryption-endpoint-refuses-plaintext-resp-in-vpc
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache.elasticache-cluster-mode-slot-routing-moved-and-crossslot|run#12|AssertionError|assert len(by_ip) == _N_SHARDS, (
Issue-Key: elasticache.global-replication-group-topology-lifecycle|_wait_for_rg_deleted#24|Timeout|time.sleep(interval_s)
Issue-Key: elasticache.transit-encryption-endpoint-refuses-plaintext-resp-in-vpc|_teardown_network#10|Timeout|time.sleep(interval_s)
Issue-Commit: 5b88798ccff0ec7f765809a75ef2c7dfb1d7aad0

# ElastiCache: 3 net-new causes — cluster-mode shards all share one backend address, and the primary-only Global-datastore and transit-encryption-required flows never converge

> _Note: this issue was identified against the code base at commit `5b88798ccff0ec7f765809a75ef2c7dfb1d7aad0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateReplicationGroup

## Description
Three net-new root causes in the elasticache emulator, all surviving the suite's already-filed/owned markings. Each cause below is independent and must be closed on its own; re-replay the check ids listed under each cause's Traces.

=== Cause 1 of 3: a cluster-mode-enabled replication group's shards are one backend wearing N names ===
AWS: Smithy (com.amazonaws.elasticache) models ReplicationGroup.NodeGroups as a list of NodeGroup, each with its own NodeGroupId, Slots keyspace and NodeGroupMembers (NodeGroupMember.CacheClusterId / CacheNodeId); NodeGroup.PrimaryEndpoint is documented as "The endpoint of the primary node in this node group (shard)". A group created with NumNodeGroups=N therefore has N distinct member cache clusters, and DescribeCacheClusters(ShowCacheNodeInfo=true) advertises a DISTINCT CacheNodes[0].Endpoint.Address per member — N shard names that resolve to N different node addresses. On the data plane that partition is what makes CLUSTER SLOTS report N ranges totalling 0-16383, a key sent to a non-owner answer `-MOVED <slot> <ip>:<port>` naming the owning NODE (not the configuration endpoint, never the contacted node itself), and a multi-key MSET spanning two slots answer `-CROSSSLOT Keys in request don't hash to the same slot` while hash-tagged keys succeed with `+OK`.
Emulator: the control plane's shard bookkeeping is accepted (NodeGroups count and the Slots partition pass), but every shard's member cache-cluster endpoint resolves to ONE address at the in-VPC vantage point: the 3 shard node names ('0001','0002','0003') resolved to 1 distinct address, i.e. all three endpoint names point at a single cache backend. (The concrete address observed in this environment is a per-environment value and is not part of the requirement; the requirement is that the N shard endpoints resolve to N distinct backends.) The check fails before CLUSTER SLOTS / MOVED / CROSSSLOT can even be exercised, so the whole slot-routing contract is unverifiable and demonstrably unimplemented: one flat keyspace is served under N shard names.
Why: a cluster-aware client bootstrapping from the configuration endpoint is told a topology the emulator does not have. Keys never redirect, so code that must handle `-MOVED` is never exercised; cross-slot multi-key commands are accepted locally that real ElastiCache refuses with `-CROSSSLOT`, so an application ships hash-tag bugs that only surface against AWS; and per-shard capacity/isolation behaviour cannot be modelled at all.
Fix: the replication-group state/provisioning component must materialise one cache-node backend per node group and advertise each member cache cluster's own endpoint address, and the served cache engine needs the cluster-mode command surface (CLUSTER SLOTS reporting the per-shard ranges it actually built, CRC16 slot ownership with `-MOVED` redirects to the owning node, and `-CROSSSLOT` refusal for multi-key commands spanning slots).
Traces: elasticache.elasticache-cluster-mode-slot-routing-moved-and-crossslot (assertion at the shard-address fan-out: "the 3 shard node names resolved to only 1 distinct addresses").

=== Cause 2 of 3: the primary-only Global datastore lifecycle never converges ===
AWS: Smithy models GlobalReplicationGroup with Status, GlobalReplicationGroupDescription and Members (GlobalReplicationGroupMemberList). A Global datastore with only its primary member settles at Status 'primary-only' ('available' needs an associated secondary region); ModifyGlobalReplicationGroup with ApplyImmediately=true re-settles the group and the new GlobalReplicationGroupDescription sticks; DisassociateGlobalReplicationGroup on the PRIMARY is refused with error code InvalidParameterValue and the message "Cannot disassociate primary replication group. Primary Replication Group will be disassociated when you delete the Global Replication Group." (fixed text, no variable parts), and the primary REMAINS in Members; after DeleteGlobalReplicationGroup(RetainPrimaryReplicationGroup=true) the primary's GlobalReplicationGroupInfo clears and the retained primary can then be deleted.
Emulator: the check is killed by the harness at its 60 s per-check budget with no answer to assert on — "Check timed out after 60.0s". The check's own waits poll DescribeReplicationGroups / DescribeGlobalReplicationGroups on 15 s and 20 s intervals and return on the first read that shows the target state, so the whole budget being burned means at least one awaited state is never reported: the replication group never reads 'available', or the global group never reads 'available'/'primary-only' after CreateGlobalReplicationGroup or after the ApplyImmediately modify. Hypothesis: a global-replication-group status that stays in its transitional value (or a modify that leaves the group non-settling) parks every subsequent poll. None of the downstream requirements — the description sticking, the InvalidParameterValue refusal text above, the primary staying in Members, the retained-primary delete — is reached, so all of them are unverified.
Why: a client driving a Global datastore polls for 'primary-only' before it may modify, disassociate or delete; against this emulator that poll never returns, so every Global-datastore workflow hangs rather than progressing, and the primary-disassociation refusal a client depends on to distinguish "delete the datastore" from "disassociate a secondary" is never produced.
Fix: the global-replication-group state machine (its create/modify status transitions and the DescribeGlobalReplicationGroups projection) plus the DisassociateGlobalReplicationGroup and DeleteGlobalReplicationGroup handlers' state/refusal paths. Related but NOT covering this check: open issue 20261112T190032Z-elasticache-migration-and-global-datastore-state-machine-diverge.md (it carries global-replication-group-secondary-lifecycle, not this topology/primary-only flow) and 20261114T131431Z-elasticache-transient-lifecycle-windows-are-not-observable-creating.md.
Traces: elasticache.global-replication-group-topology-lifecycle (timed out after 60.0s before any assertion).

=== Cause 3 of 3: the TransitEncryptionMode='required' endpoint flow never converges ===
AWS: CreateReplicationGroup with TransitEncryptionEnabled=true and TransitEncryptionMode='required' (Smithy: ReplicationGroup.TransitEncryptionEnabled, TransitEncryptionMode; GlobalReplicationGroup.TransitEncryptionEnabled documents in-transit encryption) yields a group whose DescribeReplicationGroups reports TransitEncryptionEnabled=true, TransitEncryptionMode='required' and AtRestEncryptionEnabled=true, exposes a PrimaryEndpoint and (cluster mode disabled) NO ConfigurationEndpoint, serves RESP over TLS on that endpoint, and — because the mode is 'required', not 'preferred' — gives a PLAINTEXT RESP connection on the same host and port no usable reply at all: a plaintext PING never answers `+PONG` and a plaintext SET never lands.
Emulator: the check is killed by the harness at its 60 s per-check budget — "Check timed out after 60.0s" — with no control-plane projection and no probe result to assert on. The check's waits are poll loops that return on the first satisfying read (replication group 'available' on a 2 s interval, the in-VPC executor reaching State=Active on a 2 s interval, then a bounded RESP-connect retry loop), so a 60 s burn means one of those never satisfies: the group never reads 'available', the VPC-attached in-VPC executor never reaches Active, or the advertised cache endpoint never accepts a connection so the retry loop spins to the budget. Hypothesis: the in-VPC data plane is the stall — the same non-convergence already reported for other data-plane checks. Consequently neither the required-mode control-plane projection nor the plaintext-refusal behaviour is produced.
Why: a client that asks for transit encryption 'required' and then verifies it (TLS works, plaintext is refused) gets nothing it can observe here; worse, a target that cannot be shown to refuse plaintext lets code and compliance checks pass locally that would behave differently against AWS, where plaintext on a 'required' endpoint is silently dropped.
Fix: the replication-group create/status path for transit-encryption-enabled groups, the DescribeReplicationGroups projection of TransitEncryptionEnabled/TransitEncryptionMode/PrimaryEndpoint, and the served cache endpoint's TLS/plaintext enforcement on the data plane (plus whatever keeps a VPC-attached in-VPC executor from reaching a usable state). Related but NOT covering this check: open issues 20261108T152921Z-elasticache-in-vpc-data-plane-unverifiable-the-emulator.md and 20261111T005655Z-elasticache-data-plane-still-unreachable-and-multi-wait.md.
Traces: elasticache.transit-encryption-endpoint-refuses-plaintext-resp-in-vpc (timed out after 60.0s before any assertion).

## Evidence
Suite: 150/194 passed (77.3%); elasticache 132/161 (82%). Of the 44 failures, 41 are marked OWNED/ALREADY FILED and skipped; the 3 below survived.

Cause 1 — elasticache.elasticache-cluster-mode-slot-routing-moved-and-crossslot [mismatch]:
  AssertionError: [runner-in-vpc] the 3 shard node names resolved to only 1 distinct addresses:
  {'0001': '127.0.0.1', '0002': '127.0.0.1', '0003': '127.0.0.1'}. Distinct shards sharing one
  address is a single backend wearing three names, which is the flat-keyspace failure this check
  exists to catch
  Expected: a group created with NumNodeGroups=3 advertises 3 member cache clusters whose
  CacheNodes[0].Endpoint.Address resolve to 3 DISTINCT node addresses, after which CLUSTER SLOTS
  reports 3 ranges partitioning 0-16383, a key sent to a non-owner answers `-MOVED <slot> <ip>:<port>`
  pointing at the owning node, and a two-slot MSET answers `-CROSSSLOT` while a hash-tagged MSET
  answers `+OK`.
  Actual: all 3 shard endpoint names resolve to a single address (one backend), so slot routing,
  MOVED and CROSSSLOT are never reached. (The literal address value is environment-specific.)

Cause 2 — elasticache.global-replication-group-topology-lifecycle [mismatch]:
  Error: Check timed out after 60.0s
  Expected: CreateReplicationGroup (cache.m5.large, NumNodeGroups=1, ReplicasPerNodeGroup=0) reaches
  Status 'available'; CreateGlobalReplicationGroup reaches 'primary-only' (or 'available');
  ModifyGlobalReplicationGroup(ApplyImmediately=true) re-settles with the new description;
  DisassociateGlobalReplicationGroup on the primary fails with Code 'InvalidParameterValue' and
  Message exactly "Cannot disassociate primary replication group. Primary Replication Group will be
  disassociated when you delete the Global Replication Group." with the primary still in Members;
  DeleteGlobalReplicationGroup(RetainPrimaryReplicationGroup=true) clears the primary's
  GlobalReplicationGroupInfo and the primary then deletes to ReplicationGroupNotFoundFault.
  Actual: no state wait ever satisfies within the 60 s budget (the check's poll loops return on the
  first satisfying read, so the burn is non-convergence, not sleep overhead); nothing downstream of
  the first unsatisfied wait is produced.

Cause 3 — elasticache.transit-encryption-endpoint-refuses-plaintext-resp-in-vpc [mismatch]:
  Error: Check timed out after 60.0s
  Expected: the group reports TransitEncryptionEnabled=true, TransitEncryptionMode='required',
  AtRestEncryptionEnabled=true, a PrimaryEndpoint and no ConfigurationEndpoint; RESP over TLS works
  on that endpoint; a plaintext RESP PING on the same host:port never yields `+PONG\r\n` and a
  plaintext SET does not land.
  Actual: the check is killed at the 60 s budget before any projection or probe result exists — the
  group never reads 'available', the in-VPC executor never reaches State=Active, or the advertised
  endpoint never accepts a connection.

## Steps to Reproduce
Clone the repo, boot the emulator and replay these three behavioral checks:

1. elasticache.elasticache-cluster-mode-slot-routing-moved-and-crossslot
   - CreateCacheSubnetGroup over the run's VPC subnets; CreateReplicationGroup with Engine='redis',
     NumNodeGroups=3, ReplicasPerNodeGroup=0, CacheNodeType='cache.t3.micro' and a cluster-mode
     (cluster-on) parameter group; wait Status='available'.
   - DescribeReplicationGroups: confirm ClusterEnabled=true, a ConfigurationEndpoint, 3 NodeGroups
     whose Slots partition 0-16383.
   - For each NodeGroup member, DescribeCacheClusters(ShowCacheNodeInfo=true) and resolve
     CacheNodes[0].Endpoint.Address from inside the VPC: the 3 names must resolve to 3 DISTINCT
     addresses. Observed: 1 distinct address for all 3 shards.
   - Then (currently unreachable): CLUSTER SLOTS must report 3 ranges; GET/SET of a key owned by
     another shard must answer `-MOVED <crc16-slot> <owner-ip>:<port>`; MSET over two differently
     slotted keys must answer `-CROSSSLOT` with neither key written; an MSET over two keys sharing a
     hash tag must answer `+OK`.

2. elasticache.global-replication-group-topology-lifecycle
   - CreateReplicationGroup (CacheNodeType='cache.m5.large', NumNodeGroups=1,
     ReplicasPerNodeGroup=0, AutomaticFailoverEnabled=false) and poll DescribeReplicationGroups for
     Status='available'.
   - CreateGlobalReplicationGroup(PrimaryReplicationGroupId=<rg>) and poll
     DescribeGlobalReplicationGroups(ShowMemberInfo=true) for Status in {'available','primary-only'}.
   - ModifyGlobalReplicationGroup(ApplyImmediately=true, GlobalReplicationGroupDescription=<new>),
     re-poll, assert the new description stuck.
   - DisassociateGlobalReplicationGroup(GlobalReplicationGroupId=<grg>, ReplicationGroupId=<rg>,
     ReplicationGroupRegion=<region>): expect InvalidParameterValue with the fixed message above and
     the primary still listed in Members.
   - DeleteGlobalReplicationGroup(RetainPrimaryReplicationGroup=true), wait until the primary's
     GlobalReplicationGroupInfo clears, then DeleteReplicationGroup and wait for
     ReplicationGroupNotFoundFault.
   - Observed: the check never gets past its status waits and is killed at 60 s.

3. elasticache.transit-encryption-endpoint-refuses-plaintext-resp-in-vpc
   - Build a VPC/subnets/security group, a cache subnet group, and an in-VPC executor attached to
     those subnets.
   - CreateReplicationGroup with Engine='redis', Port=6379, TransitEncryptionEnabled=true,
     TransitEncryptionMode='required', AtRestEncryptionEnabled=true, SnapshotRetentionLimit=0; wait
     Status='available'.
   - DescribeReplicationGroups: assert TransitEncryptionEnabled=true,
     TransitEncryptionMode='required', AtRestEncryptionEnabled=true, a PrimaryEndpoint and no
     ConfigurationEndpoint.
   - From inside the VPC, run RESP over TLS to the PrimaryEndpoint (must work), then PLAINTEXT
     PING + SET to the same host:port: no reply may be `+PONG\r\n` and the key must not exist.
   - Observed: the check is killed at 60 s before any of this is observable.
