Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: secretsmanager.replication-lifecycle
Issue-Shape-Id: com.amazonaws.secretsmanager#secretsmanager
Issue-Key: secretsmanager.replication-lifecycle|run#8|AssertionError|assert stop_message == (
Issue-Commit: c817d132dac12b2959b1490cc2289f359c16fb32

# StopReplicationToReplica on a primary/standalone secret still returns an emulator-authored InvalidParameterException message

> _Note: this issue was identified against the code base at commit `c817d132dac12b2959b1490cc2289f359c16fb32`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopReplicationToReplica

## Description
REOPENS 20261025T120559Z-secretsmanager-stopreplicationtoreplica-on-a-primary-standalone-secret-returns-an.md: its fix left the call refused. One root cause. Cause count: 1.

=== Cause 1 of 1: the InvalidParameterException message StopReplicationToReplica emits for a non-replica secret is not AWS's wording ===

AWS: StopReplicationToReplica is the replica-side promotion call and is only meaningful when SecretId names a REPLICA secret and the call is made in that replica's Region. Smithy (com-amazonaws-secretsmanager-secretsmanager): StopReplicationToReplica input SecretId (SecretIdType, required, @length 1..2048), output ARN (SecretARNType, @length 20..2048); modeled errors InvalidParameterException (client, httpError 400), InvalidRequestException (400), ResourceNotFoundException (400), InternalServiceError (500). Called against a primary or a standalone (unreplicated) secret, AWS refuses with HTTP 400, Error.Code InvalidParameterException and Error.Message "Operation not permitted on a primary/standalone secret." That sentence is entirely FIXED text: it has no variable part and interpolates no secret name, ARN or Region from the request or from state. The model carries no prose for this case, so the sentence is recorded behavior only; the 400 status and the InvalidParameterException code are modeled.

Emulator: the status and the error code are correct (the check's assertion on stop_code passed), but the message body is the emulator's own sentence: "Operation not permitted on a primary secret. Call must be made in replica secret's region." It renames the resource class AWS names ("primary secret" for AWS's "primary/standalone secret") and appends a second sentence about the caller's Region that AWS never emits for this refusal. This is the same divergence the fixed issue above claimed to close; the call is still refused with the emulator's wording.

Why: an error message is part of the contract. A client, test or runbook that branches on, logs or asserts the "this secret is not a replica" refusal text sees text AWS never produces; worse, the appended "Call must be made in replica secret's region" mis-diagnoses a standalone secret (which has no replica Region at all) as a wrong-Region call, sending a caller to retry elsewhere instead of concluding the secret is not a replica. It is also the first assertion of the replication-lifecycle check, so everything after it (re-replication idempotence, the replica-blocked DeleteSecret refusal, RemoveRegionsFromReplication convergence, force delete reaching not-found) is never exercised.

Fix: the StopReplicationToReplica handler's refusal path for a primary/standalone secret in the secretsmanager service crate — keep the 400 / InvalidParameterException classification and emit AWS's fixed sentence verbatim, with no request- or state-derived interpolation.

Traces: secretsmanager.replication-lifecycle (mismatch), assertion at check line 81 on the StopReplicationToReplica message.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, check [mismatch] secretsmanager.replication-lifecycle:

  File "check_secretsmanager_replication_lifecycle.py", line 81, in run
    assert stop_message == (
        "Operation not permitted on a primary/standalone secret."
    ), f"StopReplicationToReplica on a non-replica secret returned message {stop_message!r}"
  AssertionError: StopReplicationToReplica on a non-replica secret returned message "Operation not permitted on a primary secret. Call must be made in replica secret's region."

Expected (AWS): HTTP 400, Error.Code = InvalidParameterException, Error.Message = "Operation not permitted on a primary/standalone secret." — fixed text, no variable parts.
Actual (emulator): HTTP 400, Error.Code = InvalidParameterException (matches; the preceding assertion on stop_code passed), Error.Message = "Operation not permitted on a primary secret. Call must be made in replica secret's region."

The divergence is confined to the message body: the fixed sentence is reworded and an extra Region sentence is appended. No per-run value (secret name, ARN, Region, request id, timestamp) is involved on either side.

## Steps to Reproduce
1. Boot the emulator from this checkout and point a secretsmanager client at it.
2. CreateSecret(Name=<fresh name>, SecretString="value") — the secret is standalone, with no replica Regions.
3. Call StopReplicationToReplica(SecretId=<that name>) and catch the ClientError.
4. Read response["Error"]["Code"] and response["Error"]["Message"].
   Observed: Code = InvalidParameterException (correct), Message = "Operation not permitted on a primary secret. Call must be made in replica secret's region."
   Expected: Code = InvalidParameterException, Message = "Operation not permitted on a primary/standalone secret."
5. The same refusal must hold for a PRIMARY secret with live replicas: ReplicateSecretToRegions(SecretId=<name>, AddReplicaRegions=[{"Region": <a second Region>}]) then StopReplicationToReplica(SecretId=<name>) — AWS answers the identical fixed sentence.
