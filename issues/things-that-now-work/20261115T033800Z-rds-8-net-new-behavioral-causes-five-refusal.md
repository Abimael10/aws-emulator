Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.backtrack-cluster-wrong-state-rejected rds.cluster-snapshot-restore-share-over-quota-rejected rds.delete-cluster-with-members rds.delete-tenant-database-boundary rds.deploy-db-cluster-cfn-behaves rds.modify-apply-immediately-false-pending-modified-values rds.e2e-certificate-rotation-consequence-flow rds.modify-apply-immediately-true-clears-a-prior-deferred-class-change rds.global-cluster-failover-and-switchover-rejected
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.backtrack-cluster-wrong-state-rejected|run#13|AssertionError|assert message == expected, f"unexpected message: {message!r} (expected {expected!r})"
Issue-Key: rds.cluster-snapshot-restore-share-over-quota-rejected|run#21|AssertionError|assert echoed_self_add == [[]], (
Issue-Key: rds.delete-cluster-with-members|run#16|AssertionError|assert message == (
Issue-Key: rds.delete-tenant-database-boundary|run#14|AssertionError|assert wrong_arch_message == (
Issue-Key: rds.deploy-db-cluster-cfn-behaves|run#19|AssertionError|assert endpoint.lower().startswith(cluster_id.lower()), endpoint
Issue-Key: rds.modify-apply-immediately-false-pending-modified-values|run#17|AssertionError|assert live["DBInstanceStatus"] == "available", (
Issue-Key: rds|RebootDBInstance|InvalidDBInstanceState
Issue-Key: rds.modify-apply-immediately-true-clears-a-prior-deferred-class-change|run#22|AssertionError|assert final.get("PendingModifiedValues") == {}, (
Issue-Key: rds.global-cluster-failover-and-switchover-rejected|run#22|AssertionError|assert err.get("Message") == expected, f"expected message {expected!r}, got {err.get('Message')!r}"
Issue-Commit: 58910a13d43d3b73c2360b68a02a8155ad53be29

# rds: 8 net-new behavioral causes — five refusal/representation messages are emulator-authored, a deferred ModifyDBInstance drives the instance into 'modifying' (and then blocks RebootDBInstance), DescribeDBInstances omits an empty PendingModifiedValues, a cluster Endpoint is a literal 127.0.0.1, and a snapshot restore-share echoes the owner's own account

> _Note: this issue was identified against the code base at commit `58910a13d43d3b73c2360b68a02a8155ad53be29`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BacktrackDBCluster, ModifyDBClusterSnapshotAttribute, DeleteDBCluster, DeleteTenantDatabase, DescribeDBClusters, ModifyDBInstance, DescribeDBInstances, SwitchoverGlobalCluster

## Description
Eight net-new root causes in the emulated RDS control plane, all grounded in failing behavioral checks of this suite. They are independent and each needs its own fix; none is covered by an ALREADY FILED stub issue or by an OWNED-by replay key.

Cause 1 of 8: BacktrackDBCluster's not-enabled refusal carries emulator-authored prose.
AWS: Smithy BacktrackDBCluster errors InvalidDBClusterStateFault (awsQueryError code InvalidDBClusterStateFault, httpResponseCode 400). Backtrack can only be enabled at create/restore time (BacktrackWindow), and on a cluster created without it AWS refuses the call 400 InvalidDBClusterStateFault with the message 'Backtrack is not currently available to this cluster.' — that text is entirely fixed, it interpolates no identifier (recorded behavior confirms the same wording).
Emulator: same code, but the message is 'DB cluster <DBClusterIdentifier> does not have backtracking enabled' — reworded and with the cluster id spliced in where AWS has no variable part.
Why: a client that classifies or surfaces refusals by message text (and any assertion/alarm keyed on AWS's fixed sentence) sees an unknown refusal for a condition AWS words one way everywhere.
Fix: the refusal message in the BacktrackDBCluster handler's state validation (services/service-com-amazonaws-rds-amazonrdsv19/src/ops/backtrack_db_cluster.rs).
Traces: rds.backtrack-cluster-wrong-state-rejected.

Cause 2 of 8: ModifyDBClusterSnapshotAttribute lets the snapshot's OWNER account into its own 'restore' attribute.
AWS: Smithy ModifyDBClusterSnapshotAttribute -> DBClusterSnapshotAttributesResult. Adding the calling account's own account id to the 'restore' attribute is ACCEPTED (200, not a refusal), but the owner is never a grantee of its own snapshot: the call's own echoed DBClusterSnapshotAttributes entry for AttributeName='restore' comes back with an EMPTY AttributeValues, and every subsequent DescribeDBClusterSnapshotAttributes read stays empty.
Emulator: the echoed 'restore' attribute lists the owner account id (the check saw a one-element list holding the run's own account), i.e. the owner is stored as a grantee.
Why: a client reading the share list to decide who may restore sees a phantom grantee, and share-auditing logic that treats a non-empty 'restore' list as 'this snapshot is shared' misreports an unshared snapshot as shared.
Fix: grantee filtering in the ModifyDBClusterSnapshotAttribute handler/state (…/src/ops/modify_db_cluster_snapshot_attribute.rs) — drop the owning account from ValuesToAdd rather than persisting it.
Traces: rds.cluster-snapshot-restore-share-over-quota-rejected (the over-quota leg of the same check, SharedSnapshotQuotaExceeded + 'A snapshot cannot be shared with more than 20 customers.', passed).

Cause 3 of 8: DeleteDBCluster's 'still has members' refusal message is reworded.
AWS: Smithy DeleteDBCluster errors InvalidDBClusterStateFault (400). Deleting a cluster that still has a non-deleting member instance is refused with the fixed text "Cluster can't be deleted. It still contains DB instances in a non-deleting state." (two sentences, contraction 'can't', no identifier interpolated).
Emulator: 400 InvalidDBClusterStateFault with 'Cluster cannot be deleted, it still contains DB instances in non-deleting state.' — one comma-spliced sentence, 'cannot' for "can't", and the article 'a' dropped before 'non-deleting state'.
Why: same as Cause 1 — message-keyed clients and fixtures do not recognize the refusal, while the status and code look right.
Fix: the refusal message in DeleteDBCluster's member-state validation (…/src/ops/delete_db_cluster.rs). The rest of this check's contract (code, cluster untouched, member still listed, delete succeeds after the member is gone) already holds.
Traces: rds.delete-cluster-with-members.

Cause 4 of 8: DeleteTenantDatabase on a real, non-CDB instance answers the tenant-not-found message instead of the unsupported-operation one.
AWS: Smithy DeleteTenantDatabase models DBInstanceNotFoundFault / TenantDatabaseNotFoundFault / InvalidDBInstanceStateFault. On an EXISTING single-tenant (non-CDB) instance AWS does not treat the request as a missing tenant: it refuses 400 InvalidParameterCombination with "The DeleteTenantDatabase operation isn't supported for DB instance <DBInstanceIdentifier>." — fixed text with the requested instance identifier as its only variable part.
Emulator: the code is InvalidParameterCombination (correct) but the message is the tenant-lookup text "The specified tenant database wasn't found in the DB instance." — it never names the instance and states the wrong reason.
Why: a client cannot distinguish 'this instance has no such tenant' from 'this instance is not multi-tenant at all', and loses the identifier AWS puts in the message. The same check's nonexistent-instance leg (DBInstanceNotFound / 'DBInstance not found: <id>') already passes, so only this boundary is wrong.
Fix: the architecture check in the DeleteTenantDatabase handler's validation (…/src/ops/delete_tenant_database.rs) — refuse non-CDB instances with the unsupported-operation message before the tenant lookup.
Traces: rds.delete-tenant-database-boundary.

Cause 5 of 8: a provisioned Aurora DB cluster's Endpoint.Address is the literal loopback address, not an identifier-derived cluster hostname.
AWS: an Aurora cluster's writer endpoint is a DNS name whose first label is the cluster identifier — '<DBClusterIdentifier>.cluster-<suffix>.<region>.rds.amazonaws.com' — so the published Endpoint is always case-insensitively prefixed by the cluster's own identifier. (The specific host/suffix is environment-specific and is not the requirement; the identifier-derived first label is.)
Emulator: DescribeDBClusters publishes Endpoint '127.0.0.1' for a cluster created through a CloudFormation AWS::RDS::DBCluster + DBInstance stack — a bare address carrying no identifier at all.
Why: any client that resolves the writer by name, or that correlates an endpoint back to its cluster (connection-string templating, CFN outputs, service discovery), cannot tell one cluster's endpoint from another's, and a multi-cluster deployment collapses to one address.
Why this is net-new: the open issues 20261111T060457Z-rds-secretsmanager-describedbclusters-returns-a-literal-host-port-in-readerendpoint.md and 20261111T194847Z-rds-7-net-new-causes-cluster-writer-reader.md describe the same class of defect but do not carry this check's replay key, so this failure is not covered by them; a fixer should confirm the same fix closes all three.
Fix: cluster endpoint minting in the DB cluster state/serializer (the Endpoint/ReaderEndpoint construction behind CreateDBCluster/DescribeDBClusters in services/service-com-amazonaws-rds-amazonrdsv19/src), so the address is derived from the cluster identifier.
Traces: rds.deploy-db-cluster-cfn-behaves.

Cause 6 of 8: ModifyDBInstance with ApplyImmediately=false moves the instance to 'modifying', which also makes a following RebootDBInstance refuse.
AWS: Smithy ModifyDBInstance takes ApplyImmediately and returns DBInstance. A deferred (ApplyImmediately=false) change that would otherwise cause an outage — DBInstanceClass, AllocatedStorage, CACertificateIdentifier — is PARKED: DBInstanceStatus stays 'available', the live value is unchanged, and the requested value appears only in PendingModifiedValues until the preferred maintenance window or an explicit reboot. Because the instance stays 'available', RebootDBInstance is accepted (Smithy RebootDBInstance's InvalidDBInstanceStateFault is for states outside available/storage-optimization/storage-initialization/incompatible-credentials/incompatible-parameters).
Emulator: the deferred modify records PendingModifiedValues correctly and leaves the live value alone, but it flips DBInstanceStatus to 'modifying' and leaves it there. A RebootDBInstance issued next is then refused 400 InvalidDBInstanceState with 'Can only reboot db instances with state in: available, storage-optimization, storage-initialization, incompatible-credentials, incompatible-parameters.  Instance <DBInstanceIdentifier> has state: modifying.'
Why: 'modifying' tells a client an outage-causing change is in flight when nothing is being applied, so wait-for-available loops stall, and the documented way to apply a pending change (reboot) is refused — the pending change can never be applied at all.
Fix: the deferred branch of the ModifyDBInstance handler (…/src/ops/modify_db_instance.rs) — record the pending values without transitioning instance status when ApplyImmediately is false.
Traces: rds.modify-apply-immediately-false-pending-modified-values, rds.e2e-certificate-rotation-consequence-flow.

Cause 7 of 8: DescribeDBInstances omits PendingModifiedValues entirely when there is nothing pending.
AWS: DBInstance always carries a PendingModifiedValues element in the query/XML response; when no change is pending it is serialized empty, so an SDK parses it to an empty structure ({}), never to absent.
Emulator: after an ApplyImmediately=true ModifyDBInstance applies every requested change (class and storage both live, status back to 'available'), the member is absent from the response, so the SDK yields None instead of {}.
Why: a client that reads instance['PendingModifiedValues'] — or that compares it to {} to decide 'settled' — raises a KeyError / sees a null where AWS guarantees a structure, which is exactly the probe used to confirm a modify has fully landed.
Fix: the DBInstance serializer (the PendingModifiedValues member must always be emitted, empty when nothing is pending).
Traces: rds.modify-apply-immediately-true-clears-a-prior-deferred-class-change.

Cause 8 of 8: SwitchoverGlobalCluster's 'target is already the primary' refusal message is emulator-authored.
AWS: Smithy SwitchoverGlobalCluster models DBClusterNotFoundFault / GlobalClusterNotFoundFault / InvalidDBClusterStateFault / InvalidGlobalClusterStateFault; naming the CURRENT primary as the switchover target is refused as a parameter-combination error, 400 InvalidParameterCombination, with the fixed text 'The specified switchover target is already the primary DB cluster.' — no identifier is interpolated.
Emulator: the code is InvalidParameterCombination (correct) but the message is 'DB cluster <DBClusterIdentifier> is already the primary cluster of global cluster <GlobalClusterIdentifier>' — reworded, with two identifiers spliced in where AWS has none.
Why: message-keyed classification of this refusal fails, and the wording implies a global-cluster state fault rather than a bad-parameter combination. The same check's FailoverGlobalCluster leg (DBClusterNotFoundFault, "A DB cluster with ARN <arn> doesn't exist. Choose another failover target and try again.") already passes, so only the switchover message diverges.
Fix: the refusal message in the SwitchoverGlobalCluster handler's target validation (…/src/ops/switchover_global_cluster.rs).
Traces: rds.global-cluster-failover-and-switchover-rejected.

## Evidence
Cause 1 — rds.backtrack-cluster-wrong-state-rejected:
  expected: InvalidDBClusterStateFault / 'Backtrack is not currently available to this cluster.'
  actual:   InvalidDBClusterStateFault / 'DB cluster sim2real-backtrack-a91f89394b3f does not have backtracking enabled'
  (AssertionError: unexpected message: ... (expected 'Backtrack is not currently available to this cluster.'))

Cause 2 — rds.cluster-snapshot-restore-share-over-quota-rejected:
  ModifyDBClusterSnapshotAttribute(AttributeName='restore', ValuesToAdd=[<own account>]) ->
  expected echoed DBClusterSnapshotAttributes[restore].AttributeValues == [] ; actual == ['<own account>']
  (AssertionError: expected the owner-account share to echo an empty restore attribute, got [['000000000000']])

Cause 3 — rds.delete-cluster-with-members:
  expected: "Cluster can't be deleted. It still contains DB instances in a non-deleting state."
  actual:   'Cluster cannot be deleted, it still contains DB instances in non-deleting state.'
  (code InvalidDBClusterStateFault matched)

Cause 4 — rds.delete-tenant-database-boundary:
  DeleteTenantDatabase(DBInstanceIdentifier=<existing non-CDB postgres instance>, TenantDBName='tenant1') ->
  expected: InvalidParameterCombination / "The DeleteTenantDatabase operation isn't supported for DB instance <db_id>."
  actual:   InvalidParameterCombination / "The specified tenant database wasn't found in the DB instance."

Cause 5 — rds.deploy-db-cluster-cfn-behaves:
  expected: DescribeDBClusters Endpoint starts (case-insensitively) with the cluster identifier
  actual:   Endpoint == '127.0.0.1'
  (AssertionError: 127.0.0.1, at `assert endpoint.lower().startswith(cluster_id.lower())`)

Cause 6 — rds.modify-apply-immediately-false-pending-modified-values:
  ModifyDBInstance(DBInstanceClass=db.t3.small, ApplyImmediately=False) on an available db.t3.micro ->
  PendingModifiedValues.DBInstanceClass == 'db.t3.small' and live class 'db.t3.micro' (both correct), but
  expected DBInstanceStatus 'available'; actual 'modifying'
  (AssertionError: a deferred class change must not start a resize: the instance must stay 'available', got 'modifying')
  rds.e2e-certificate-rotation-consequence-flow (same cause, downstream effect):
  ModifyDBInstance(CACertificateIdentifier=<other RSA CA>, ApplyImmediately=False) then RebootDBInstance ->
  InvalidDBInstanceStateFault: 'Can only reboot db instances with state in: available, storage-optimization, storage-initialization, incompatible-credentials, incompatible-parameters.  Instance sim2real-cacert-578a3e0260cc has state: modifying.'

Cause 7 — rds.modify-apply-immediately-true-clears-a-prior-deferred-class-change:
  after ModifyDBInstance(DBInstanceClass=db.t3.small, AllocatedStorage=25, ApplyImmediately=True) settled to
  available/db.t3.small/25 GiB, expected PendingModifiedValues == {} ; actual None (member absent from the response)

Cause 8 — rds.global-cluster-failover-and-switchover-rejected:
  SwitchoverGlobalCluster(GlobalClusterIdentifier=<gc>, TargetDbClusterIdentifier=<current primary arn>) ->
  expected: InvalidParameterCombination / 'The specified switchover target is already the primary DB cluster.'
  actual:   InvalidParameterCombination / 'DB cluster sim2real-behav-cl-8f8089df5405 is already the primary cluster of global cluster sim2real-behav-gc-8cdc84815a01'
  (the FailoverGlobalCluster not-found leg of the same check passed)

Identifiers, account ids and host names quoted above are per-run values that fill the templates; only the fixed text and the request/state-derived variable parts named in the description are requirements.

## Steps to Reproduce
All against the emulator's rds endpoint (service token 'rds', crate services/service-com-amazonaws-rds-amazonrdsv19).

Cause 1: CreateDBCluster(Engine=aurora-mysql, no BacktrackWindow) -> wait available -> BacktrackDBCluster(DBClusterIdentifier=<id>, BacktrackTo=now-1min, Force=true). Compare the InvalidDBClusterStateFault message to 'Backtrack is not currently available to this cluster.'

Cause 2: CreateDBCluster(aurora-mysql) -> CreateDBClusterSnapshot -> wait available -> ModifyDBClusterSnapshotAttribute(AttributeName='restore', ValuesToAdd=[<the caller's own account id>]). Inspect the returned DBClusterSnapshotAttributesResult and then DescribeDBClusterSnapshotAttributes: the 'restore' AttributeValues must be empty in both.

Cause 3: CreateDBCluster(aurora-mysql) -> wait available -> CreateDBInstance(Engine=aurora-mysql, DBClusterIdentifier=<cluster>) -> wait available -> DeleteDBCluster(DBClusterIdentifier=<cluster>, SkipFinalSnapshot=true). Compare the message to "Cluster can't be deleted. It still contains DB instances in a non-deleting state."

Cause 4: CreateDBInstance(Engine=postgres, single-tenant) -> wait available -> DeleteTenantDatabase(DBInstanceIdentifier=<that instance>, TenantDBName='tenant1'). Expect InvalidParameterCombination with "The DeleteTenantDatabase operation isn't supported for DB instance <that instance>."

Cause 5: deploy a CloudFormation stack with AWS::RDS::DBCluster + AWS::RDS::DBInstance, then DescribeDBClusters(DBClusterIdentifier=<cluster>) and read Endpoint: its first DNS label must be the cluster identifier, not a bare address.

Cause 6: CreateDBInstance(Engine=postgres, DBInstanceClass=db.t3.micro) -> wait available -> ModifyDBInstance(DBInstanceClass=db.t3.small, ApplyImmediately=False) -> DescribeDBInstances: DBInstanceStatus must still be 'available' with the new class only under PendingModifiedValues. Then (certificate variant) ModifyDBInstance(CACertificateIdentifier=<another RSA CA from DescribeCertificates>, ApplyImmediately=False) followed by RebootDBInstance — the reboot must be accepted.

Cause 7: on the same instance, ModifyDBInstance(DBInstanceClass=db.t3.small, AllocatedStorage=25, ApplyImmediately=True); poll DescribeDBInstances until available with both values live, then assert PendingModifiedValues is present and equal to an empty structure.

Cause 8: CreateGlobalCluster(aurora-mysql) -> CreateDBCluster(..., GlobalClusterIdentifier=<gc>) -> wait available/membership 1 -> SwitchoverGlobalCluster(GlobalClusterIdentifier=<gc>, TargetDbClusterIdentifier=<that cluster's ARN>). Compare the InvalidParameterCombination message to 'The specified switchover target is already the primary DB cluster.'
