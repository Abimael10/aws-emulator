Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift.scheduled-action-duplicate-create-rejected redshift.cluster-iam-roles-association-quota-exceeded redshift.list-recommendations-max-records-and-unknown-cluster-rejected redshift.snapshot-copy-grant-default-key-is-the-aws-managed-redshift-key redshift.reboot-is-gated-by-cluster-state-across-a-pause-resume-cycle redshift.reserved-node-exchange-well-formed-id-not-found redshift.restore-table-from-cluster-snapshot-rejects-an-unknown-source-table-name redshift.snapshot-schedule-duplicate-create-and-modify-unknown-rejected
Issue-Key: redshift|CreateScheduledAction|ClusterNotFound
Issue-Key: redshift.cluster-iam-roles-association-quota-exceeded|_expect_error#17|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: redshift|ListRecommendations|InvalidParameterValue
Issue-Key: redshift.snapshot-copy-grant-default-key-is-the-aws-managed-redshift-key|run#9|AssertionError|assert created["KmsKeyId"] in (managed_key["Arn"], managed_key["KeyId"]), (
Issue-Key: redshift.reboot-is-gated-by-cluster-state-across-a-pause-resume-cycle|run#28|AssertionError|assert message == expected_message, (
Issue-Key: redshift.reserved-node-exchange-well-formed-id-not-found|run#14|AssertionError|assert message == expected_status, f"expected message {expected_status!r}, got {message!r}"
Issue-Key: redshift.restore-table-from-cluster-snapshot-rejects-an-unknown-source-table-name|run#20|AssertionError|assert message == (
Issue-Key: redshift.snapshot-schedule-duplicate-create-and-modify-unknown-rejected|run#9|AssertionError|assert message == expected_dup, f"expected message {expected_dup!r}, got {message!r}"
Issue-Commit: a4215905cca27c128686d10d799e4c7536c2e42a

# redshift: 8 net-new causes — CreateScheduledAction refuses a not-yet-existing target cluster, ModifyClusterIamRoles has no 50-role ceiling, ListRecommendations rejects MaxRecords=0, SnapshotCopyGrant default key is unresolved, and four refusal messages are emulator-authored

> _Note: this issue was identified against the code base at commit `a4215905cca27c128686d10d799e4c7536c2e42a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateScheduledAction

## Description
Eight distinct net-new root causes under the redshift behavioral heading. Every other failing redshift check is either OWNED by an existing open issue or ALREADY FILED against an unimplemented stub (DeleteSnapshotSchedule) and is excluded here.

Cause 1 of 8: CreateScheduledAction refuses a PauseCluster TargetAction whose ClusterIdentifier does not exist yet.
  AWS: Smithy: CreateScheduledAction models ClusterNotFoundFault (awsQueryError ClusterNotFound, httpError 404) among its errors, but the recorded behavior shows AWS does NOT apply it to the TargetAction's ClusterIdentifier at create time — the recording creates a scheduled action whose PauseCluster target names a cluster that was never created and the call succeeds, returning Schedule equal to the submitted cron expression and State 'ACTIVE'. The target cluster is resolved at invocation time, not at registration.
  Emulator: the call is refused with HTTP 404 ClusterNotFound / 'Cluster <clusterIdentifier> not found.', so the check cannot even reach its duplicate-name assertion.
  Why: a client cannot register a schedule before, or independently of, the cluster it targets — the ordinary IaC pattern of declaring schedules alongside (or ahead of) cluster creation is impossible, and the caller sees a 404 for a request AWS accepts.
  Fix: the CreateScheduledAction handler's validation — stop resolving TargetAction.*.ClusterIdentifier against cluster state at registration.
  Traces: redshift.scheduled-action-duplicate-create-rejected.

Cause 2 of 8: ModifyClusterIamRoles enforces no per-cluster IAM-role ceiling, so a role past the limit is accepted.
  AWS: Smithy: ModifyClusterIamRoles.AddIamRoles targets IamRoleArnList; the model carries no list length bound, so the quota is recorded behavior only. The recording shows associating exactly the ceiling (50 roles) succeeds, and adding one more is refused with InvalidParameterValue and the message 'Cluster already has <associatedCount> IAM Roles. Only <remaining> new IAM Role(s) may be added to this Cluster.' Fixed text: 'Cluster already has ', ' IAM Roles. Only ', ' new IAM Role(s) may be added to this Cluster.'; variable parts: the count already associated and the remaining headroom, both read from the cluster's state (0 at the ceiling).
  Emulator: the over-ceiling ModifyClusterIamRoles returns HTTP 200 with the full Cluster object (ClusterStatus 'modifying'), associating the extra role instead of refusing.
  Why: a client relying on the quota refusal to detect that it has exhausted its role headroom never sees it, and the cluster ends up holding more associated roles than AWS permits — state that cannot be reproduced on real AWS and that silently changes COPY/UNLOAD authorization behavior.
  Fix: the ModifyClusterIamRoles handler's validation (count the cluster's existing IamRoles plus AddIamRoles against the ceiling and refuse before mutating state).
  Traces: redshift.cluster-iam-roles-association-quota-exceeded.

Cause 3 of 8: ListRecommendations rejects MaxRecords=0 and reports the wrong allowed range.
  AWS: Smithy: ListRecommendations.MaxRecords is an IntegerOptional with no range trait, and it is the operation's paginated pageSize. Recorded behavior: MaxRecords=0 is accepted and answers with an empty Recommendations list; a positive page size is refused with InvalidParameterValue and the message 'Invalid value for max records. Must be between <min> and <max>' where the bounds are the operation's own — the recording shows 0 and 0. Fixed text: 'Invalid value for max records. Must be between ', ' and '; variable parts: the two bounds, which must be the bounds this operation actually enforces.
  Emulator: MaxRecords=0 is itself refused, with InvalidParameterValue / 'Invalid value for max records. Must be between 20 and 100' — a generic 20..100 page-size rule borrowed from other Describe* operations.
  Why: the one page size AWS accepts is rejected, so the operation is unreachable for a client that pins MaxRecords, and a client that reads the advertised bounds out of the message is told to retry with a value AWS itself refuses.
  Fix: the ListRecommendations handler's MaxRecords validation and its refusal message (per-operation bounds, not the shared Describe* page-size check).
  Traces: redshift.list-recommendations-max-records-and-unknown-cluster-rejected.

Cause 4 of 8: CreateSnapshotCopyGrant with no KmsKeyId reports the literal alias string instead of resolving the AWS-managed Redshift key.
  AWS: Smithy: CreateSnapshotCopyGrant.KmsKeyId is optional; SnapshotCopyGrant.KmsKeyId is the key the grant is for. Recorded behavior: omitting KmsKeyId grants Redshift permission on the account's AWS-managed key behind the fixed alias 'alias/aws/redshift' (KeyManager 'AWS'), and both CreateSnapshotCopyGrant and DescribeSnapshotCopyGrants report that key's id or ARN — never the alias string.
  Emulator: the created grant reports KmsKeyId == 'alias/aws/redshift', the alias literal, where the resolved key id or 'arn:<partition>:kms:<region>:<account>:key/<keyId>' is required.
  Why: a client that feeds the grant's KmsKeyId into a KMS call, a key policy or a cross-region snapshot-copy configuration gets an alias name where a key identifier is required, and cannot correlate the grant with the key kms:DescribeKey reports for that alias.
  Fix: the CreateSnapshotCopyGrant handler's state — resolve the default alias to the account's AWS-managed Redshift key at create time and persist/report that key's identifier, so the grant's own describe agrees.
  Traces: redshift.snapshot-copy-grant-default-key-is-the-aws-managed-redshift-key.

Cause 5 of 8: RebootCluster's InvalidClusterState message is emulator-authored.
  AWS: Smithy: RebootCluster models InvalidClusterStateFault (awsQueryError InvalidClusterState, httpError 400). Recorded behavior: rebooting a cluster that is not available is refused with that code and the message 'Cluster <clusterIdentifier> is not in available state to reboot.' Fixed text: 'Cluster ' and ' is not in available state to reboot.'; variable part: the cluster identifier from the request. Notably the message does NOT name the current state.
  Emulator: same code, different sentence — 'Cluster <clusterIdentifier> is in state <state>; it must be available to be rebooted.'
  Why: a client matching the refusal text (the common way to distinguish a state gate from other InvalidClusterState causes such as an in-flight resize) does not recognise it.
  Fix: the RebootCluster handler's state-gate refusal message.
  Traces: redshift.reboot-is-gated-by-cluster-state-across-a-pause-resume-cycle.

Cause 6 of 8: DescribeReservedNodeExchangeStatus's ReservedNodeNotFound message uses the wrong member wording.
  AWS: Smithy: DescribeReservedNodeExchangeStatus models ReservedNodeNotFoundFault (awsQueryError ReservedNodeNotFound, httpError 404) and takes ReservedNodeId. Recorded behavior: AWS words this operation's refusal per the member it was given — 'ReservedNodeId <reservedNodeId> not found.' — which differs from the 'Reserved Node <reservedNodeId> not found.' wording GetReservedNodeExchangeOfferings and AcceptReservedNodeExchange use for the same fault. Fixed text: 'ReservedNodeId ' and ' not found.'; variable part: the id from the request.
  Emulator: emits the sibling operations' wording, 'Reserved Node <reservedNodeId> not found.', for DescribeReservedNodeExchangeStatus — one shared message for all three operations.
  Why: a client that distinguishes which reserved-node operation refused, or matches the documented per-operation text, mis-attributes the failure. (20261029T200555Z-redshift-namespace-reserved-node-idc-application-hsm-refusals.md covers other reserved-node wording but does not carry this check's replay key.)
  Fix: the DescribeReservedNodeExchangeStatus handler's not-found refusal message.
  Traces: redshift.reserved-node-exchange-well-formed-id-not-found.

Cause 7 of 8: RestoreTableFromClusterSnapshot's InvalidTableRestoreArgument message is a generic multi-parameter sentence instead of AWS's source-table-specific one.
  AWS: Smithy: RestoreTableFromClusterSnapshot models InvalidTableRestoreArgumentFault (awsQueryError InvalidTableRestoreArgument, httpError 400) and takes SourceDatabaseName/SourceSchemaName/SourceTableName. Recorded behavior: when only SourceTableName is absent from the snapshot, AWS names that table and suggests the nearest table the snapshot does contain: 'Source table "<sourceTableName>" doesn\'t exist in snapshot. Consider using: "<candidateTableName>"'. Fixed text: 'Source table "', '" doesn\'t exist in snapshot. Consider using: "', '"'; variable parts: the SourceTableName from the request and the candidate table name read from the snapshot's own catalog.
  Emulator: refuses with the right code but the generic combined-parameter sentence "The value specified for the sourceDatabaseName, sourceSchemaName, or sourceTableName parameter, or a combination of these, doesn't exist in the snapshot." — it never names the offending table nor offers a candidate.
  Why: a client (or operator) cannot tell which of the three source parameters was wrong, and loses the suggestion AWS supplies, so automated remediation that reads the candidate name out of the message has nothing to read.
  Fix: the RestoreTableFromClusterSnapshot handler's validation — distinguish an unknown source table from an unknown database/schema and compose the per-table message with a candidate from the snapshot's table list.
  Traces: redshift.restore-table-from-cluster-snapshot-rejects-an-unknown-source-table-name.

Cause 8 of 8: CreateSnapshotSchedule's SnapshotScheduleAlreadyExists message is emulator-authored.
  AWS: Smithy: CreateSnapshotSchedule models SnapshotScheduleAlreadyExistsFault (awsQueryError SnapshotScheduleAlreadyExists, httpError 400). Recorded behavior: the message is 'Snapshot schedule <scheduleIdentifier> already exists.' Fixed text: 'Snapshot schedule ' and ' already exists.'; variable part: the ScheduleIdentifier from the request.
  Emulator: the code is right but the sentence is its own: "Cannot create the snapshot schedule because a schedule with the identifier '<scheduleIdentifier>' already exists."
  Why: a client matching AWS's already-exists wording to implement idempotent create-or-reuse does not recognise the refusal and treats it as an unknown failure. (20261108T233629Z-redshift-snapshot-schedule-lifecycle-diverges-missing-invalidschedule-floor.md mentions already-exists wording but does not carry this check's replay key.)
  Fix: the CreateSnapshotSchedule handler's duplicate-identifier refusal message.
  Traces: redshift.snapshot-schedule-duplicate-create-and-modify-unknown-rejected.

## Evidence
Cause 1 — redshift.scheduled-action-duplicate-create-rejected:
  Expected: CreateScheduledAction(ScheduledActionName=<name>, TargetAction={'PauseCluster': {'ClusterIdentifier': <never-created cluster>}}, Schedule='cron(0 10 * * ? *)', IamRole=<role arn>) succeeds with Schedule == the submitted cron and State == 'ACTIVE'.
  Actual: botocore.errorfactory.ClusterNotFoundFault: An error occurred (ClusterNotFound) when calling the CreateScheduledAction operation: Cluster <clusterIdentifier> not found.

Cause 2 — redshift.cluster-iam-roles-association-quota-exceeded:
  Expected: after 50 roles are associated, ModifyClusterIamRoles(AddIamRoles=[<51st role arn>]) -> InvalidParameterValue / 'Cluster already has 50 IAM Roles. Only 0 new IAM Role(s) may be added to this Cluster.'
  Actual: AssertionError: expected the call to be refused, but it succeeded: {'Cluster': {'ClusterIdentifier': ..., 'ClusterStatus': 'modifying', ...}} — HTTP 200 and the role associated.

Cause 3 — redshift.list-recommendations-max-records-and-unknown-cluster-rejected:
  Expected: ListRecommendations(MaxRecords=0) -> 200 with Recommendations == []; ListRecommendations(MaxRecords=1) -> InvalidParameterValue / 'Invalid value for max records. Must be between 0 and 0'.
  Actual: ClientError (InvalidParameterValue) when calling ListRecommendations: 'Invalid value for max records. Must be between 20 and 100' — raised on the MaxRecords=0 call.

Cause 4 — redshift.snapshot-copy-grant-default-key-is-the-aws-managed-redshift-key:
  Expected: created['KmsKeyId'] in (managed_key['Arn'], managed_key['KeyId']) where managed_key = kms.DescribeKey(KeyId='alias/aws/redshift').KeyMetadata (KeyManager 'AWS'), i.e. 'arn:aws:kms:<region>:<account>:key/<keyId>' or '<keyId>'.
  Actual: 'alias/aws/redshift' — the alias literal.

Cause 5 — redshift.reboot-is-gated-by-cluster-state-across-a-pause-resume-cycle:
  Expected (code InvalidClusterState): 'Cluster <clusterIdentifier> is not in available state to reboot.'
  Actual: 'Cluster <clusterIdentifier> is in state paused; it must be available to be rebooted.'

Cause 6 — redshift.reserved-node-exchange-well-formed-id-not-found:
  Expected (code ReservedNodeNotFound, DescribeReservedNodeExchangeStatus(ReservedNodeId=<id>)): 'ReservedNodeId <id> not found.'
  Actual: 'Reserved Node <id> not found.'

Cause 7 — redshift.restore-table-from-cluster-snapshot-rejects-an-unknown-source-table-name:
  Expected (code InvalidTableRestoreArgument): 'Source table "<sourceTableName>" doesn\'t exist in snapshot. Consider using: "<candidateTableName>"'
  Actual: "The value specified for the sourceDatabaseName, sourceSchemaName, or sourceTableName parameter, or a combination of these, doesn't exist in the snapshot."

Cause 8 — redshift.snapshot-schedule-duplicate-create-and-modify-unknown-rejected:
  Expected (code SnapshotScheduleAlreadyExists): 'Snapshot schedule <scheduleIdentifier> already exists.'
  Actual: "Cannot create the snapshot schedule because a schedule with the identifier '<scheduleIdentifier>' already exists."

## Steps to Reproduce
Cause 1: create an IAM role trusted by scheduler.redshift.amazonaws.com, then call CreateScheduledAction with TargetAction={'PauseCluster':{'ClusterIdentifier': <a name no cluster uses>}}, Schedule='cron(0 10 * * ? *)', IamRole=<that role arn>. Observe the 404 ClusterNotFound; AWS returns the action with State 'ACTIVE'.

Cause 2: create a single-node cluster, wait for 'available', create 51 IAM roles, ModifyClusterIamRoles with the first 50 (succeeds), then ModifyClusterIamRoles(AddIamRoles=[the 51st]). Observe HTTP 200 instead of the InvalidParameterValue quota refusal; DescribeClusters then shows 51 associated roles.

Cause 3: call ListRecommendations(MaxRecords=0). Observe InvalidParameterValue 'Invalid value for max records. Must be between 20 and 100' instead of a 200 with an empty Recommendations list. Then call ListRecommendations(MaxRecords=1) and observe the same 20..100 bounds instead of 'Must be between 0 and 0'.

Cause 4: call kms.DescribeKey(KeyId='alias/aws/redshift') to obtain the AWS-managed key, then CreateSnapshotCopyGrant with a name and no KmsKeyId. Read SnapshotCopyGrant.KmsKeyId from the response and from DescribeSnapshotCopyGrants. Observe the alias literal rather than the key id/ARN.

Cause 5: create a single-node cluster, wait for 'available', take a manual snapshot, PauseCluster and wait for 'paused', then RebootCluster. Compare the InvalidClusterState message with 'Cluster <id> is not in available state to reboot.'

Cause 6: call DescribeReservedNodeExchangeStatus(ReservedNodeId=<a well-formed, unowned uuid>). Compare the ReservedNodeNotFound message with 'ReservedNodeId <id> not found.'

Cause 7: create a cluster, create a table in its database, take a manual snapshot and wait for 'available', then call RestoreTableFromClusterSnapshot with the real SourceDatabaseName/SourceSchemaName and a SourceTableName no table uses. Compare the InvalidTableRestoreArgument message with the 'Source table "..." doesn\'t exist in snapshot. Consider using: "..."' form.

Cause 8: CreateSnapshotSchedule(ScheduleIdentifier=<id>, ScheduleDefinitions=['rate(12 hours)']), then call it again with the same identifier and ScheduleDefinitions=['rate(24 hours)']. Compare the SnapshotScheduleAlreadyExists message with 'Snapshot schedule <id> already exists.'
