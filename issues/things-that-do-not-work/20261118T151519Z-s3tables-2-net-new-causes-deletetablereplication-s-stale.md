Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3tables.delete-table-replication-stale-version-token-rejected s3tables.delete-table-replication-without-permission-denied
Issue-Shape-Id: com.amazonaws.s3tables#S3TableBuckets
Issue-Key: s3tables.delete-table-replication-stale-version-token-rejected|run#13|AssertionError|assert code == "ConflictException", (
Issue-Key: s3tables.delete-table-replication-without-permission-denied|run#14|AssertionError|assert code == "AccessDeniedException", f"expected AccessDeniedException, got {code}"
Issue-Commit: e811b8b41259ffda648a98493ef74b5680a725e4

# s3tables: 2 net-new causes — DeleteTableReplication's stale-versionToken ConflictException and its unauthorized AccessDeniedException are both unreachable because the operation answers NotImplemented

> _Note: this issue was identified against the code base at commit `e811b8b41259ffda648a98493ef74b5680a725e4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteTableReplication

## Description
DeleteTableReplication never reaches any of its modeled refusal paths: every call answers a NotImplemented error, so the two distinct precondition/authorization behaviours below are both unobservable. A scaffold-stub issue naming `DeleteTableReplication` and `services/service-com-amazonaws-s3tables-s3tablebuckets/src/ops/delete_table_replication.rs` has been filed for the bare unimplemented handler; this issue records the two refusal semantics that handler must implement, which that stub issue does not state. Two related open issues look similar but cover neither cause: 20261102T065133Z-s3tables-encryption-metrics-replication-put-operations-answer-501.md covers only the Put* operations, and 20261115T014715Z-s3tables-authorization-is-never-evaluated-identity-based-denies.md enumerates 4 other refusal paths and not DeleteTableReplication.

Cause 1 of 2: DeleteTableReplication with a superseded versionToken is not refused with ConflictException.
AWS: Smithy — DeleteTableReplication (DELETE /table-replication, success code 204) takes required httpQuery members `tableArn` (TableARN, @length 1..2048, ARN pattern) and `versionToken` (String), and models ConflictException @httpError 409 among its errors. PutTableReplication returns a required `versionToken` in its output and advances it on each successful put, so a token captured from an earlier put is superseded. The recording shows the superseded-token delete refused 409 ConflictException with message "Provided version token does not match the table version token." — that message is entirely fixed text for this error class (it carries no request- or state-derived values). The recording also shows the refusal has no effect: a following GetTableReplication still returns the configuration at the current versionToken, and a delete issued with the current token then succeeds (204) after which GetTableReplication answers 404 NotFoundException with "The specified replication configuration does not exist."
Emulator: the delete returns error code 'NotImplemented' instead of ConflictException, so neither the precondition refusal, its message, its no-effect guarantee, nor the subsequent successful delete can be observed.
Why: a client doing optimistic concurrency on a table's replication configuration cannot distinguish a lost-update conflict from a server-side gap. Against AWS a stale token is safely rejected and the caller re-reads and retries; against the emulator every delete fails identically, so concurrency-control code is never exercised and a tool that treats the failure as retryable loops forever.
Fix: the DeleteTableReplication handler and its version-token precondition check in the s3tables replication state — the handler must compare the request's versionToken with the table's stored replication version token, refuse 409 ConflictException with the fixed message above when they differ while leaving the stored configuration and token untouched, and on a match delete the configuration and answer 204 so a later GetTableReplication returns 404 NotFoundException.
Traces: s3tables.delete-table-replication-stale-version-token-rejected.

Cause 2 of 2: DeleteTableReplication from a principal with no s3tables permissions is not denied with AccessDeniedException.
AWS: Smithy — DeleteTableReplication models AccessDeniedException @httpError 403. The recording shows a restricted assumed-role principal's delete refused 403 AccessDeniedException whose message follows AWS's standard identity-based-deny template: the fixed text is "User: ", " is not authorized to perform: ", " on resource: ", " because no identity-based policy allows the ", " action"; the variable parts are the calling principal's assumed-role ARN (account id, role name and session name from the request's credentials), the action `s3tables:DeleteTableReplication`, and the target table ARN from the request. No value from any single run (a particular role name, session name or account) is part of the requirement.
Emulator: the restricted principal's delete returns error code 'NotImplemented' rather than AccessDeniedException, so authorization is never evaluated on this route and the deny — plus the guarantee that the denied call leaves the configuration's role and versionToken unchanged — cannot be observed.
Why: a caller validating least-privilege policies sees an indistinguishable failure for a permitted and a forbidden principal, so IAM regression tests over s3tables replication pass or fail for the wrong reason, and an emulator-backed CI cannot catch an over-broad policy.
Fix: authorization evaluation for the DeleteTableReplication route (the handler must consult the identity-based policy before touching state) together with the same handler's refusal serializer, so the deny is a 403 AccessDeniedException carrying AWS's identity-based-deny message template and the configuration is left intact.
Traces: s3tables.delete-table-replication-without-permission-denied.

## Evidence
Expected (AWS, per the recordings and the Smithy model):
- delete-table-replication-stale-version-token-rejected: DeleteTableReplication(tableArn, versionToken=<superseded token from the first PutTableReplication>) -> 409 ConflictException, Message exactly "Provided version token does not match the table version token."; GetTableReplication still returns versionToken == <current token>; DeleteTableReplication(versionToken=<current token>) -> 204; GetTableReplication -> 404 NotFoundException, Message "The specified replication configuration does not exist."
- delete-table-replication-without-permission-denied: DeleteTableReplication called by an assumed role with no s3tables permissions -> 403 AccessDeniedException, Message "User: arn:aws:sts::<account>:assumed-role/<role-name>/<session-name> is not authorized to perform: s3tables:DeleteTableReplication on resource: <table ARN> because no identity-based policy allows the s3tables:DeleteTableReplication action" (fixed text as quoted; <account>, <role-name>, <session-name> and <table ARN> come from the request/credentials); GetTableReplication afterwards still reports the original configuration.role and versionToken.

Actual (emulator replay):
- "AssertionError: DeleteTableReplication with a superseded versionToken returned 'NotImplemented', expected \"ConflictException\"" (check_s3tables_delete_table_replication_stale_version_token_rejected.py line 50)
- "AssertionError: expected AccessDeniedException, got NotImplemented" (check_s3tables_delete_table_replication_without_permission_denied.py line 44)

In both cases the error code returned for DeleteTableReplication is 'NotImplemented', i.e. the route answers an unimplemented-operation error rather than any modeled s3tables error, so the refusal status (409 / 403), the error code and the error message all diverge. The handler in question is services/service-com-amazonaws-s3tables-s3tablebuckets/src/ops/delete_table_replication.rs.

## Steps to Reproduce
Cause 1:
1. CreateTableBucket (source) and CreateTableBucket (destination); CreateNamespace and CreateTable(format=ICEBERG) in the source bucket.
2. Create an IAM role trusted by replication.s3tables.amazonaws.com with an inline s3tables:* allow policy.
3. PutTableReplication(tableArn, configuration={role, rules:[{destinations:[{destinationTableBucketARN: <dest>}]}]}) and keep the returned versionToken as STALE.
4. PutTableReplication again with versionToken=STALE and the same configuration; keep the new returned versionToken as CURRENT (it must differ from STALE).
5. DeleteTableReplication(tableArn, versionToken=STALE) — AWS refuses 409 ConflictException "Provided version token does not match the table version token."; the emulator returns error code NotImplemented.
6. GetTableReplication(tableArn) — versionToken must still be CURRENT; then DeleteTableReplication(tableArn, versionToken=CURRENT) must succeed (204) and GetTableReplication must then answer 404 NotFoundException "The specified replication configuration does not exist."

Cause 2:
1. Create source and destination table buckets, a namespace and an ICEBERG table, and a replication role as above.
2. PutTableReplication(tableArn, configuration) and read the current versionToken with GetTableReplication.
3. Assume a role that grants no s3tables permissions and, with that client, call DeleteTableReplication(tableArn, versionToken=<current>) — AWS refuses 403 AccessDeniedException with the identity-based-deny message naming the assumed-role ARN, s3tables:DeleteTableReplication and the table ARN; the emulator returns error code NotImplemented.
4. With the permitted principal, GetTableReplication must still show configuration.role unchanged, and DeleteTableReplication with the current token must then succeed.
