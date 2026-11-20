Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3tables.delete-table-replication-stale-version-token-rejected
Issue-Shape-Id: com.amazonaws.s3tables#S3TableBuckets
Issue-Key: s3tables.delete-table-replication-stale-version-token-rejected|run#14|AssertionError|assert message == "Provided version token does not match the table version token.", message
Issue-Commit: 8ab6f9a0d0e86ce0ebe92394c517fc3eaca43f78

# s3tables DeleteTableReplication stale-versionToken ConflictException carries the configuration-mismatch sentence instead of AWS's table-version-token wording

> _Note: this issue was identified against the code base at commit `8ab6f9a0d0e86ce0ebe92394c517fc3eaca43f78`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteTableReplication

## Description
Cause 1 of 1: DeleteTableReplication refuses a superseded versionToken with the right class and status (409 ConflictException) but states it with the wrong sentence, so the refusal message diverges from AWS.

AWS: Smithy — DeleteTableReplication (DELETE /table-replication, success code 204) takes required httpQuery members `tableArn` (TableARN, @length 1..2048, ARN pattern `^(arn:aws[-a-z0-9]*:[a-z0-9]+:[-a-z0-9]*:[0-9]{12}:bucket/[a-z0-9_-]{3,63}/table/[a-zA-Z0-9-_]{1,255})$`) and `versionToken` (String), and models ConflictException @httpError 409 ("The request failed because there is a conflict with a previous write. You can retry the request."). The model does not carry the wording, so this is recorded behavior: the recording shows a delete presenting a versionToken that an intervening successful PutTableReplication has superseded refused 409 ConflictException with Message exactly "Provided version token does not match the table version token." That whole sentence is FIXED text for this error class — it has no variable parts at all: neither the presented token, the current token, the table ARN nor any other request- or state-derived value appears in it. The recording also shows the refusal has no effect (GetTableReplication still reports the configuration at the current versionToken) and that a delete with the current token then succeeds with 204, after which GetTableReplication answers 404 NotFoundException "The specified replication configuration does not exist."

Emulator: the status (409) and the error code (ConflictException) are right, but the Message is "The specified version token does not match the configuration." The check asserts the message and fails: `AssertionError: The specified version token does not match the configuration.` The handler reaches for the wrong one of the two version-token refusal sentences the service's state module defines: it states the refusal with the configuration-mismatch sentence where this operation's refusal is about the TABLE's version token. Because the message assertion fires first, the recording's no-effect guarantee (GetTableReplication still at the current token) and the positive control (delete with the current token succeeds 204, then GetTableReplication is 404 NotFoundException) are left unverified on this route by this check and should be re-confirmed by the fix.

Why: the error message is part of the refusal contract. A client doing optimistic concurrency on a table's replication configuration — or a test, a log assertion or an operator runbook that distinguishes a stale-token conflict from any other 409 by the sentence AWS returns — matches on text the emulator never produces, so the conflict is misclassified or reported with wording an AWS-targeted consumer does not recognise, even though the code and status happen to agree.

Why this is net-new: the open issue 20261118T151519Z-s3tables-2-net-new-causes-deletetablereplication-s-stale.md recorded this behaviour while the operation answered NotImplemented and therefore keyed on the error-code assertion; the operation is now implemented and answers ConflictException, so that key no longer matches and the surviving divergence is the message text alone. The fixed issue 20261118T151519Z-s3tables-deletetablereplication-stub.md stood the handler up; this is not a refusal of a call that should have succeeded, so it does not reopen it.

Fix: the DeleteTableReplication handler's version-token precondition refusal — it must state the 409 ConflictException with this service's TABLE version-token sentence rather than its configuration-mismatch sentence, leaving the stored configuration and token untouched, and the two constants must stay distinguished so the sibling operations that genuinely state the configuration-mismatch wording are unaffected.

Traces: s3tables.delete-table-replication-stale-version-token-rejected.

## Evidence
Expected (AWS, per the recording and the Smithy model):
  DeleteTableReplication(tableArn=<table>, versionToken=<token superseded by a later successful PutTableReplication>)
  -> HTTP 409, Error.Code "ConflictException",
     Error.Message "Provided version token does not match the table version token."  (entirely fixed text; no request- or state-derived values)
  then GetTableReplication(tableArn) -> 200 with versionToken == <current token> (refusal had no effect)
  then DeleteTableReplication(tableArn, versionToken=<current token>) -> 204
  then GetTableReplication(tableArn) -> 404 NotFoundException "The specified replication configuration does not exist."

Actual (emulator replay):
  HTTP 409, Error.Code "ConflictException" (both correct),
  Error.Message "The specified version token does not match the configuration."

  check_s3tables_delete_table_replication_stale_version_token_rejected.py line 54:
    assert message == "Provided version token does not match the table version token.", message
    AssertionError: The specified version token does not match the configuration.

Source of the divergence in the checkout: services/service-com-amazonaws-s3tables-s3tablebuckets/src/ops/delete_table_replication.rs lines 239-246 raise ConflictException with crate::state::VERSION_TOKEN_MISMATCH_MESSAGE (services/service-com-amazonaws-s3tables-s3tablebuckets/src/state.rs lines 1662-1663 = "The specified version token does not match the configuration.") where the sentence AWS returns for this operation is already defined one declaration down as crate::state::TABLE_VERSION_TOKEN_MISMATCH_MESSAGE (state.rs lines 1670-1671 = "Provided version token does not match the table version token.").

## Steps to Reproduce
1. CreateTableBucket (source) and CreateTableBucket (destination).
2. CreateNamespace in the source bucket, then CreateTable(format=ICEBERG) in it.
3. Create an IAM role trusted by replication.s3tables.amazonaws.com with an inline s3tables:* allow policy.
4. PutTableReplication(tableArn, configuration={role, rules:[{destinations:[{destinationTableBucketARN: <dest bucket ARN>}]}]}) and keep the returned versionToken as STALE.
5. PutTableReplication again with versionToken=STALE and the same configuration; keep the new returned versionToken as CURRENT (it must differ from STALE).
6. DeleteTableReplication(tableArn, versionToken=STALE): AWS refuses 409 ConflictException with Message "Provided version token does not match the table version token."; the emulator refuses 409 ConflictException with Message "The specified version token does not match the configuration."
7. Re-confirm the behaviour the message assertion masked: GetTableReplication(tableArn) must still report versionToken == CURRENT, then DeleteTableReplication(tableArn, versionToken=CURRENT) must answer 204 and GetTableReplication(tableArn) must then answer 404 NotFoundException "The specified replication configuration does not exist."
