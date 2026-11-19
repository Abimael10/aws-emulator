Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3tables.put-table-replication-cross-account-without-permission-denied
Issue-Shape-Id: com.amazonaws.s3tables#S3TableBuckets
Issue-Key: s3tables.put-table-replication-cross-account-without-permission-denied|run#15|AssertionError|assert broken_status == "failed", (
Issue-Commit: 030d08d3bec65b1ba1c11a2b3a61a7c1792f100f

# s3tables table-replication backfill never fails: a role S3 Tables cannot assume still settles at COMPLETED, so GetTableReplicationStatus reports a broken replication as healthy

> _Note: this issue was identified against the code base at commit `030d08d3bec65b1ba1c11a2b3a61a7c1792f100f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTableReplicationStatus

## Description
Cause 1 of 1: the replication state a table reports for each destination is derived from the destination table bucket's existence and the clock alone — the configuration's `role` is never evaluated — so a replication configuration naming a role S3 Tables cannot assume (and which holds no s3tables permissions) reports `completed` where AWS reports `failed`.

AWS: Smithy — `PutTableReplication` (PUT /table-replication, success 200) takes a required `configuration` of `TableReplicationConfiguration` whose `role` is the IAM role the service assumes to replicate, and the operation's own documentation describes the role as one "passed to replication.s3tables.amazonaws.com". The call itself is accepted synchronously (200, required `versionToken` + `status`) regardless of whether that role is usable; the role is exercised by the asynchronous backfill. `GetTableReplicationStatus` (GET /replication-status, 200) returns a required `destinations` list of `ReplicationDestinationStatusModel`, whose required `replicationStatus` is the `ReplicationStatus` enum with exactly the values `pending`, `completed`, `failed`, and whose optional `failureMessage` is documented as: "If replication has failed, this field contains an error message describing the failure reason." The failed terminal state therefore exists in the model precisely for a backfill that cannot proceed. Recorded behavior: with a role whose trust policy trusts only an unrelated principal (`ec2.amazonaws.com`) and which carries no s3tables permissions, the destination's `replicationStatus` settles at the terminal value `failed` within the check's 150 s budget; with a role trusted by `replication.s3tables.amazonaws.com` and granted `s3tables:*`, the same destination reaches `completed` (the recording's positive control). The requirement is the class of outcome — an unassumable/unauthorized replication role must reach the terminal `failed` state, with `failureMessage` set to the service's description of that failure reason; the exact text of that message is not captured by this check, so only the state transition is asserted here.

Emulator: both configurations settle identically at `completed`. The replay asserts on the untrusted-role table and gets `AssertionError: expected the untrusted role's backfill to settle at FAILED, got 'completed'`. Reading the derivation confirms the cause is general rather than timing: `table_replication_status` (services/service-com-amazonaws-s3tables-s3tablebuckets/src/state/replication_status.rs:220-304) computes each destination's state from exactly two inputs — whether the store holds the destination table bucket (absent ⇒ `Failed` with `BUCKET_NOT_FOUND_MESSAGE`) and whether `now >= configured_at.max(table.modified_at) + REPLICATION_COMPLETION_DELAY` — and comments that "Nothing refuses a destination this store holds, so such a destination carries no failure to state", hard-wiring `failure_message: None` and a `Completed`/`Pending` binary for every destination bucket that exists. The configuration's `role` is stored but never consulted; `GetTableReplicationStatus`'s handler (src/ops/get_table_replication_status.rs:239-248) just serializes that derivation. Consequently the only reachable `failed` state is "destination bucket not held", and the role-related failure class is unreachable for every table whose destination bucket exists.

Why: a client that monitors replication health cannot distinguish a working replication from one that can never copy anything. Against AWS, `failed` plus `failureMessage` is the signal to fix the role's trust policy or permissions; against the emulator every misconfigured replication reports `completed` with a `lastSuccessfulReplicatedUpdate`, so alarm/remediation code that branches on `failed` is dead, and operators validating least-privilege replication roles get a false pass on a role S3 Tables could never assume. This is an overly-permissive divergence: the emulator accepts and then reports success for a configuration AWS reports as broken.

Fix: the replication status derivation in the s3tables replication state (`ComAmazonawsS3tablesS3tablebucketsState::table_replication_status` in src/state/replication_status.rs) together with whatever the `PutTableReplication` path records about the configuration's role, so the derived destination state also depends on whether the configured role is usable by the replication principal — i.e. whether the role's trust policy admits `replication.s3tables.amazonaws.com` and the role grants the s3tables actions replication needs (the same IAM-role resolution the service's existing authorization/assume-role machinery already models for other paths). A destination whose role fails that check must derive `ReplicationState::Failed` with `failure_message` set to the service's wording for that failure reason rather than `Completed`; the handler and serializer need no change beyond emitting the derived value.

Traces: s3tables.put-table-replication-cross-account-without-permission-denied.

Related open issues, none of which covers this cause: 20261118T155030Z-s3tables-table-replication-diverges-puttablereplication-versiontoken-form-and.md carries the PutTableReplication versionToken form and the GetTableReplication missing-configuration 404; 20261118T151519Z-s3tables-2-net-new-causes-deletetablereplication-s-stale.md and 20261118T151519Z-s3tables-deletetablereplication-stub.md cover DeleteTableReplication's refusal paths and its unimplemented stub. All three are about the replication configuration's own CRUD/refusals; none touches the replication STATUS derivation GetTableReplicationStatus answers with.

## Evidence
Check s3tables.put-table-replication-cross-account-without-permission-denied, [mismatch].

Replay output:
  File "check_s3tables_put_table_replication_cross_account_without_permission_denied.py", line 60, in run
    assert broken_status == "failed", (
  AssertionError: expected the untrusted role's backfill to settle at FAILED, got 'completed'

Expected (AWS, per the recording and the model): GetTableReplicationStatus(tableArn=<source table>) → 200, destinations[0].replicationStatus == "failed" (terminal) for a configuration whose role trusts only ec2.amazonaws.com and holds no s3tables permissions, with destinations[0].failureMessage set to the service's description of the failure reason; and destinations[0].replicationStatus == "completed" for an identical configuration whose role is trusted by replication.s3tables.amazonaws.com and granted s3tables:* (positive control, which passes).

Actual (emulator): both tables report destinations[0].replicationStatus == "completed" with no failureMessage, within the same polling budget. PutTableReplication itself returns 200 with a versionToken in both cases, which matches AWS — the divergence is entirely in the status the asynchronous backfill settles at.

Model facts used: ReplicationStatus enum = [pending, completed, failed]; ReplicationDestinationStatusModel.replicationStatus required, failureMessage documented "If replication has failed, this field contains an error message describing the failure reason."; PutTableReplication errors include AccessDeniedException/ForbiddenException but the recording shows the put is accepted, so the role is judged asynchronously, not at the put.

Code showing the cause is general: services/service-com-amazonaws-s3tables-s3tablebuckets/src/state/replication_status.rs:239-301 — `let completed = replica.is_some() && at >= completes_at;` with `failure_message: None` for every destination bucket the store holds, and `Failed` reachable only via the missing-destination-bucket branch at line 247. The configuration's `role` is never read in this function.

## Steps to Reproduce
1. CreateTableBucket twice (a source and a destination table bucket); CreateNamespace in the source bucket.
2. CreateTable (format=ICEBERG) in that namespace — call it the broken table — and a second one for the control.
3. iam.CreateRole with a trust policy allowing sts:AssumeRole to Service=ec2.amazonaws.com only, and attach no s3tables permissions at all.
4. PutTableReplication(tableArn=<broken table>, configuration={role: <that role ARN>, rules: [{destinations: [{destinationTableBucketARN: <destination bucket ARN>}]}]}). It returns 200 with a versionToken, as AWS does.
5. Poll GetTableReplicationStatus(tableArn=<broken table>) until destinations[0].replicationStatus is terminal (failed or completed), up to 150 s. AWS settles at "failed" with a failureMessage; the emulator settles at "completed" with no failureMessage.
6. Control: iam.CreateRole with a trust policy allowing sts:AssumeRole to Service=replication.s3tables.amazonaws.com, attach an inline policy allowing s3tables:* on *, PutTableReplication the second table with it, and poll — this must reach "completed" (it already does), proving the emulator does not distinguish the two roles.
