Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|CreateDBInstance|2xx->2xx|$.CreateDBInstanceResponse.CreateDBInstanceResult.DBInstance.AllocatedStorage
Issue-Key: rds|CreateDBInstance|2xx->2xx|$.CreateDBInstanceResponse.CreateDBInstanceResult.DBInstance.DBName
Issue-Key: rds|CreateDBInstance|2xx->2xx|$.CreateDBInstanceResponse.CreateDBInstanceResult.DBInstance.DbInstancePort
Issue-Key: rds|CreateDBInstance|2xx->2xx|$.CreateDBInstanceResponse.CreateDBInstanceResult.DBInstance.MasterUsername
Issue-Key: rds|CreateDBInstance|2xx->2xx|$.CreateDBInstanceResponse.CreateDBInstanceResult.DBInstance.PromotionTier
Issue-Key: rds|CreateDBInstance|2xx->2xx|$.CreateDBInstanceResponse.CreateDBInstanceResult.DBInstance.StorageEncryptionType
Issue-Key: rds|DescribeDBInstances|2xx->2xx|$.DescribeDBInstancesResponse.DescribeDBInstancesResult.DBInstances.DBInstance.AllocatedStorage
Issue-Key: rds|DescribeDBInstances|2xx->2xx|$.DescribeDBInstancesResponse.DescribeDBInstancesResult.DBInstances.DBInstance.DBName
Issue-Key: rds|DescribeDBInstances|2xx->2xx|$.DescribeDBInstancesResponse.DescribeDBInstancesResult.DBInstances.DBInstance.DbInstancePort
Issue-Key: rds|DescribeDBInstances|2xx->2xx|$.DescribeDBInstancesResponse.DescribeDBInstancesResult.DBInstances.DBInstance.MasterUsername
Issue-Key: rds|DescribeDBInstances|2xx->2xx|$.DescribeDBInstancesResponse.DescribeDBInstancesResult.DBInstances.DBInstance.PromotionTier
Issue-Key: rds|DescribeDBInstances|2xx->2xx|$.DescribeDBInstancesResponse.DescribeDBInstancesResult.DBInstances.DBInstance.StorageEncryptionType
Issue-Key: rds|DeleteDBInstance|2xx->2xx|$.DeleteDBInstanceResponse.DeleteDBInstanceResult.DBInstance.AllocatedStorage
Issue-Key: rds|DeleteDBInstance|2xx->2xx|$.DeleteDBInstanceResponse.DeleteDBInstanceResult.DBInstance.DBName
Issue-Key: rds|DeleteDBInstance|2xx->2xx|$.DeleteDBInstanceResponse.DeleteDBInstanceResult.DBInstance.DbInstancePort
Issue-Key: rds|DeleteDBInstance|2xx->2xx|$.DeleteDBInstanceResponse.DeleteDBInstanceResult.DBInstance.MasterUsername
Issue-Key: rds|DeleteDBInstance|2xx->2xx|$.DeleteDBInstanceResponse.DeleteDBInstanceResult.DBInstance.PromotionTier
Issue-Key: rds|DeleteDBInstance|2xx->2xx|$.DeleteDBInstanceResponse.DeleteDBInstanceResult.DBInstance.StorageEncryptionType
Issue-Key: rds|ModifyDBInstance|2xx->2xx|$.ModifyDBInstanceResponse.ModifyDBInstanceResult.DBInstance.DBParameterGroups.DBParameterGroup.ParameterApplyStatus
Issue-Key: rds|ModifyDBInstance|2xx->2xx|$.ModifyDBInstanceResponse.ModifyDBInstanceResult.DBInstance.Endpoint.Port
Issue-Key: rds|ModifyDBInstance|2xx->2xx|$.ModifyDBInstanceResponse.ModifyDBInstanceResult.DBInstance.HttpEndpointEnabled
Issue-Key: rds|ModifyDBInstance|2xx->2xx|$.ModifyDBInstanceResponse.ModifyDBInstanceResult.DBInstance.PendingModifiedValues
Issue-Key: rds|ModifyDBInstance|2xx->2xx|$.ModifyDBInstanceResponse.ModifyDBInstanceResult.DBInstance.StorageThroughput
Issue-Key: rds|ModifyDBInstance|2xx->2xx|$.ModifyDBInstanceResponse.ModifyDBInstanceResult.DBInstance.StorageType
Issue-Key: rds|ModifyDBInstance|2xx->2xx|$.ModifyDBInstanceResponse.ModifyDBInstanceResult.DBInstance.VPCNetworkingEnabled
Issue-Key: rds|DescribeDBInstances|2xx->2xx|$
Issue-Key: rds|DescribeDBInstances|2xx->2xx|$.DescribeDBInstancesResponse.DescribeDBInstancesResult.DBInstances.DBInstance.DBParameterGroups.DBParameterGroup.ParameterApplyStatus
Issue-Key: rds|RebootDBInstance|2xx->2xx|$.RebootDBInstanceResponse.RebootDBInstanceResult.DBInstance.DBParameterGroups.DBParameterGroup.ParameterApplyStatus
Issue-Key: rds|RebootDBInstance|2xx->2xx|$.RebootDBInstanceResponse.RebootDBInstanceResult.DBInstance.Endpoint.Port
Issue-Key: rds|RebootDBInstance|2xx->2xx|$.RebootDBInstanceResponse.RebootDBInstanceResult.DBInstance.HttpEndpointEnabled
Issue-Key: rds|RebootDBInstance|2xx->2xx|$.RebootDBInstanceResponse.RebootDBInstanceResult.DBInstance.PendingModifiedValues
Issue-Key: rds|RebootDBInstance|2xx->2xx|$.RebootDBInstanceResponse.RebootDBInstanceResult.DBInstance.StorageThroughput
Issue-Key: rds|RebootDBInstance|2xx->2xx|$.RebootDBInstanceResponse.RebootDBInstanceResult.DBInstance.StorageType
Issue-Key: rds|RebootDBInstance|2xx->2xx|$.RebootDBInstanceResponse.RebootDBInstanceResult.DBInstance.VPCNetworkingEnabled
Issue-Related: 20261022T162132Z-rds-describedbinstances-omits-modeled-members-and-reports-wrong.md
Issue-Related: 20261030T001542Z-rds-db-subnet-group-and-db-instance-projections.md
Issue-Related: 20261111T091517Z-rds-db-instance-read-delete-envelopes-and-the.md
Issue-Commit: c00bf07ce89c73b0369514a40ab33e3142015054

# rds: DB instance envelopes omit modeled DBInstance members and serve emulator storage/port defaults (6 causes)

> _Note: this issue was identified against the code base at commit `c00bf07ce89c73b0369514a40ab33e3142015054`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDBInstance

## Description
Every operation that returns a `DBInstance` serves an envelope that drops a fixed set of modeled members and substitutes the emulator's own storage/port/parameter-status values for the ones AWS derives from the instance's engine and from its pending-reboot lifecycle. Six distinct causes, all on the `rds` service.

Shared fault: the DBInstance projection is built from a generic provisioned-instance template (gp3 / 125 MiB-s throughput / in-sync parameter groups / a random local listener port) rather than from the request and the instance's actual engine and state, and a fixed set of members is never emitted at all.

--- Cause 1 of 6: CreateDBInstance on an Aurora MySQL cluster member ---
AWS: Smithy: CreateDBInstance output -> DBInstance. The on-disk DBInstance shape models AllocatedStorage (Integer), DBName (String), MasterUsername (String), PromotionTier (IntegerOptional), DbInstancePort (Integer, doc: "The port that the DB instance listens on. If the DB instance is part of a DB cluster, this can be a different port than the DB cluster port."), StorageType (String), StorageThroughput (IntegerOptional, doc: "This setting applies only to the gp3 storage type."), StorageEncryptionType (enum StorageEncryptionType: none|sse-rds|sse-kms) and PendingModifiedValues. VPCNetworkingEnabled and HttpEndpointEnabled are NOT in the on-disk DBInstance shape — recorded behavior only: the recording shows AWS emitting VPCNetworkingEnabled 'true' and HttpEndpointEnabled 'false' on every DB instance envelope. For an instance that is a member of an Aurora cluster AWS reports storage as the cluster's: StorageType 'aurora', StorageThroughput 0, AllocatedStorage 1, DbInstancePort 0 (the member inherits the cluster port, so the instance-level port element is zero), PromotionTier 0, StorageEncryptionType 'sse-rds', an empty PendingModifiedValues element, and echoes the cluster's DBName and MasterUsername.
Emulator: the CreateDBInstance envelope omits AllocatedStorage, VPCNetworkingEnabled, HttpEndpointEnabled, DBName, StorageEncryptionType, PendingModifiedValues, MasterUsername and PromotionTier entirely, and reports StorageThroughput 125 instead of 0, DbInstancePort 3306 instead of 0, StorageType 'gp3' instead of 'aurora'. Status is 200 both sides.
Why: a client that creates an Aurora writer and reads the create response back cannot learn the master username, the initial database name, the promotion tier or the encryption type, and is told the member has its own gp3 volume on port 3306 — so Aurora-vs-RDS branching, failover-priority logic and storage-cost/throughput logic all take the wrong branch.
Fix: the CreateDBInstance handler's DBInstance projection/serializer, and the instance state record it is built from (storage attributes must come from the parent cluster for a cluster member, not from a provisioned-instance default).
Traces: rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found#task#7

--- Cause 2 of 6: DescribeDBInstances for that same Aurora cluster member ---
AWS: Smithy: DescribeDBInstances -> DBInstanceMessage -> DBInstance (same shape as Cause 1), plus Endpoint (shape Endpoint, member Port). The engine port is the instance's real listener port — 3306 for an Aurora MySQL member.
Emulator: the same eight members are omitted (AllocatedStorage, VPCNetworkingEnabled, HttpEndpointEnabled, DBName, StorageEncryptionType, PendingModifiedValues, MasterUsername, PromotionTier), StorageThroughput is 125 instead of 0, DbInstancePort 3306 instead of 0, StorageType 'gp3' instead of 'aurora', and additionally Endpoint.Port publishes the emulator's ephemeral local listener port instead of the engine port 3306.
Why: the read-back path a client uses to discover how to connect advertises a port that is not the engine's, so connection strings assembled from Endpoint.Address + Endpoint.Port are wrong for anything that is not the emulator's own process-local listener. (Only the port is a requirement here; the specific ephemeral number differs per run.)
Fix: the DescribeDBInstances projection and the endpoint serializer — Endpoint.Port must be the engine port, not the bound listener port.
Traces: rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found#task#8

--- Cause 3 of 6: DeleteDBInstance envelope ---
AWS: Smithy: DeleteDBInstance output -> DBInstance. The delete response reports the instance as it was, with the same member set as Causes 1-2, and the recording shows CACertificateIdentifier serialized as an empty element for this Aurora member.
Emulator: same eight omissions and the same StorageThroughput 125 / DbInstancePort 3306 / StorageType 'gp3' / random Endpoint.Port substitutions, and additionally populates CACertificateIdentifier with 'rds-ca-rsa2048-g1' where AWS emits it empty.
Why: a client that keys teardown bookkeeping off the delete response (last-known master username, DB name, promotion tier, certificate identifier) records values the service never returned.
Fix: the DeleteDBInstance handler's DBInstance projection — it must serialize the stored record, not re-synthesize defaults.
Traces: rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found#task#17

--- Cause 4 of 6: ModifyDBInstance that switches the DB parameter group ---
AWS: Smithy: ModifyDBInstance output -> DBInstance, member DBParameterGroups -> DBParameterGroupStatusList -> DBParameterGroupStatus{DBParameterGroupName, ParameterApplyStatus}. When a modify attaches a different parameter group, the immediate response reports ParameterApplyStatus 'applying'; it is not 'in-sync' until the apply has completed. AWS also echoes the requested StorageType ('gp2'), reports StorageThroughput 0 for gp2 (StorageThroughput "applies only to the gp3 storage type"), emits an empty PendingModifiedValues element, VPCNetworkingEnabled 'true', HttpEndpointEnabled 'false', and Endpoint.Port as the engine port 3306.
Emulator: ParameterApplyStatus 'in-sync' instead of 'applying'; StorageType 'gp3' instead of the requested 'gp2'; StorageThroughput 125 instead of 0; VPCNetworkingEnabled, HttpEndpointEnabled and PendingModifiedValues omitted; Endpoint.Port is the ephemeral local listener port instead of 3306.
Why: the parameter-group apply has no observable intermediate state, so a client that waits for 'applying' -> 'pending-reboot' before rebooting sees the change as already settled and skips the reboot; and a storage-type modification silently does not take, so the instance keeps reporting gp3/125 after being moved to gp2.
Fix: the ModifyDBInstance handler — apply the requested storage attributes to state, and set the attached parameter group's ParameterApplyStatus through the modeled apply lifecycle instead of pinning 'in-sync'.
Traces: rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found#task#17

--- Cause 5 of 6: the post-modify DescribeDBInstances poll never converges ---
AWS: Smithy: DBParameterGroupStatus.ParameterApplyStatus. After a parameter-group switch completes, AWS reports the instance back at DBInstanceStatus 'available' with ParameterApplyStatus 'pending-reboot' — a static parameter change needs a reboot to take effect, so the apply status rests at 'pending-reboot' until one happens.
Emulator: ParameterApplyStatus is 'in-sync' from the moment the group is attached and never becomes 'pending-reboot', so the fixture's poll for that state runs to its 180 s cap and the step reaches no result at all (replay reports "poll capped at 180s"). The same envelope also omits VPCNetworkingEnabled/HttpEndpointEnabled/PendingModifiedValues and reports StorageType 'gp3', StorageThroughput 125 and a random Endpoint.Port.
Why: any client or IaC provider that waits for 'pending-reboot' before issuing RebootDBInstance blocks forever against the emulator; the parameter-group apply lifecycle is unobservable.
Fix: the DB instance state machine for parameter-group application (the modify handler plus whatever advances instance state), which must transition applying -> pending-reboot and hold there until a reboot.
Traces: rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found#task#18

--- Cause 6 of 6: RebootDBInstance envelope reports the parameter group already in sync ---
AWS: Smithy: RebootDBInstance output -> DBInstance. The reboot response still reports ParameterApplyStatus 'pending-reboot' (the status clears only once the reboot has completed and the instance is re-read), and carries the same member set as above: StorageType 'gp2' as configured, StorageThroughput 0, an empty PendingModifiedValues, VPCNetworkingEnabled 'true', HttpEndpointEnabled 'false', Endpoint.Port 3306.
Emulator: ParameterApplyStatus 'in-sync', StorageType 'gp3', StorageThroughput 125, VPCNetworkingEnabled / HttpEndpointEnabled / PendingModifiedValues omitted, Endpoint.Port the ephemeral local listener port.
Why: the reboot response is indistinguishable from a steady-state read, so a client cannot tell that the reboot it just requested is the one carrying the pending static parameter change.
Fix: the RebootDBInstance handler's DBInstance projection and the parameter-apply state it reads.
Traces: rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found#task#19

Related open issues that do NOT cover these steps (none of these clusters is owned by one): 20261111T091517Z-rds-db-instance-read-delete-envelopes-and-the.md, 20261117T115022Z-cloudformation-logs-rds-rds-describedbinstances-omits-promotiontier-for-an-aurora-db.md, 20261109T205028Z-rds-db-cluster-envelopes-publish-a-random-listener.md.

## Evidence
Cause 1 — rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found#task#7, CreateDBInstance, 200 vs 200:
  $...DBInstance.AllocatedStorage: expected '1', got None
  $...DBInstance.VPCNetworkingEnabled: expected 'true', got None
  $...DBInstance.StorageThroughput: expected '0', got '125'
  $...DBInstance.HttpEndpointEnabled: expected 'false', got None
  $...DBInstance.DBName: expected 'test', got None
  $...DBInstance.StorageEncryptionType: expected 'sse-rds', got None
  $...DBInstance.PendingModifiedValues: expected '' (empty element), got None
  $...DBInstance.MasterUsername: expected the request's master username, got None
  $...DBInstance.PromotionTier: expected '0', got None
  $...DBInstance.DbInstancePort: expected '0', got '3306'
  $...DBInstance.StorageType: expected 'aurora', got 'gp3'

Cause 2 — same fixture#task#8, DescribeDBInstances, 200 vs 200: the same eleven keys plus
  $...DBInstance.Endpoint.Port: expected '3306', got '45007' (the emulator's ephemeral listener port; the number itself is per-run)

Cause 3 — same fixture#task#17, DeleteDBInstance, 200 vs 200: the same keys plus
  $...DBInstance.CACertificateIdentifier: expected '' , got 'rds-ca-rsa2048-g1'

Cause 4 — rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found#task#17, ModifyDBInstance, 200 vs 200:
  $...DBInstance.DBParameterGroups.DBParameterGroup.ParameterApplyStatus: expected 'applying', got 'in-sync'
  $...DBInstance.StorageType: expected 'gp2', got 'gp3'
  $...DBInstance.StorageThroughput: expected '0', got '125'
  $...DBInstance.VPCNetworkingEnabled: expected 'true', got None
  $...DBInstance.HttpEndpointEnabled: expected 'false', got None
  $...DBInstance.PendingModifiedValues: expected '', got None
  $...DBInstance.Endpoint.Port: expected '3306', got '43857'

Cause 5 — same fixture#task#18, DescribeDBInstances, 200 vs 200:
  $: poll capped at 180s — the awaited envelope has DBInstanceStatus 'available' with DBParameterGroups.DBParameterGroup.ParameterApplyStatus 'pending-reboot'; the emulator stays 'in-sync' forever, so the step never satisfies. Same StorageType/StorageThroughput/VPCNetworkingEnabled/HttpEndpointEnabled/PendingModifiedValues/Endpoint.Port divergences as Cause 4.

Cause 6 — same fixture#task#19, RebootDBInstance, 200 vs 200:
  $...DBInstance.DBParameterGroups.DBParameterGroup.ParameterApplyStatus: expected 'pending-reboot', got 'in-sync'
  plus StorageType 'gp2'->'gp3', StorageThroughput '0'->'125', VPCNetworkingEnabled/HttpEndpointEnabled/PendingModifiedValues omitted, Endpoint.Port '3306'->'43857'

## Steps to Reproduce
1. Replay fixture rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found: create an aurora-mysql DB cluster, then CreateDBInstance a writer member of it (step task#7), DescribeDBInstances it (task#8) and DeleteDBInstance it (task#17). Compare the DBInstance element in each response: AllocatedStorage, VPCNetworkingEnabled, HttpEndpointEnabled, DBName, StorageEncryptionType, PendingModifiedValues, MasterUsername and PromotionTier are absent, StorageType is 'gp3' not 'aurora', StorageThroughput is 125 not 0, DbInstancePort is 3306 not 0, and Endpoint.Port is the emulator's listener port, not 3306.
2. Replay fixture rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found: create a gp2 mysql DB instance and a DB parameter group, ModifyDBInstance to attach that group (task#17) — the response reports ParameterApplyStatus 'in-sync' instead of 'applying' and StorageType 'gp3' instead of the configured 'gp2'.
3. Poll DescribeDBInstances for DBInstanceStatus 'available' with ParameterApplyStatus 'pending-reboot' (task#18): it never appears and the poll runs to the 180 s cap.
4. RebootDBInstance (task#19): the response reports ParameterApplyStatus 'in-sync' instead of 'pending-reboot'.
