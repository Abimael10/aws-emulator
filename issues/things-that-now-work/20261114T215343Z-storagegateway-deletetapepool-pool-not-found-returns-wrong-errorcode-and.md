Issue-Source: trace-sweep-divergence
Issue-Trace: storagegateway-tape-pool-lifecycle
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: storagegateway-tape-pool-lifecycle storagegateway-tape-pool-resource-not-found-errors
Issue-Shape-Id: com.amazonaws.storagegateway#StorageGateway_20130630
Issue-Key: storagegateway|DeleteTapePool|4xx->4xx|$.error.errorCode
Issue-Key: storagegateway|DeleteTapePool|4xx->4xx|$.message
Issue-Commit: d33d509d5311768bd1282d02eb2794d638dad9b8

# DeleteTapePool pool-not-found returns wrong errorCode and interpolated message

> _Note: this issue was identified against the code base at commit `d33d509d5311768bd1282d02eb2794d638dad9b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteTapePool

## Description
Storage Gateway (awsJson1_1, API version 2013-06-30) answers DeleteTapePool for a non-existent tape pool with the right HTTP status but the wrong InvalidGatewayRequestException payload: both the StorageGatewayError errorCode and the message text diverge from AWS.

Cause 1 of 1 - the tape-pool-not-found reply carries errorCode 'InvalidParameters' instead of 'PoolNotFound' and splices the pool ARN into a message whose AWS text is fixed (fixtures storagegateway-tape-pool-lifecycle step task#9, and storagegateway-tape-pool-resource-not-found-errors step task#1).
AWS: Smithy: storagegateway#DeleteTapePool takes PoolARN (required, @length 50..500) and declares exactly two errors, InternalServerError (httpError 500) and InvalidGatewayRequestException (httpError 400, members `message` and `error` -> StorageGatewayError{errorCode: ErrorCode enum, errorDetails}). For a syntactically valid PoolARN that names no existing tape pool, the recorded response is HTTP 400 with $.error.errorCode = 'PoolNotFound' and $.message = 'The specified pool was not found.' The whole message is FIXED text with no variable part - the identity of the missing pool is conveyed by the errorCode (and errorDetails), not by the message. The resource-kind discriminator is the errorCode, and 'InvalidParameters' is the generic bad-input code, not the not-found code. (The on-disk ErrorCode enum does not list a 'PoolNotFound' member, so for that specific value this is recorded behavior only; the modelled enum does establish that a resource-specific not-found code - e.g. TapeCartridgeNotFound, VolumeNotFound, SnapshotNotFound - is what AWS uses for a missing resource, never InvalidParameters.)
Emulator: answers HTTP 400 but with $.error.errorCode = 'InvalidParameters' and $.message = 'The tape pool <poolArn> was not found.', i.e. it classifies a missing resource as an input-validation failure and interpolates the request's PoolARN into the text.
Why: a client distinguishes "this pool is already gone / never existed" from "you sent me a malformed request" by the errorCode, since both arrive as InvalidGatewayRequestException with HTTP 400. Against the emulator an idempotent delete or a cleanup path that tolerates PoolNotFound instead sees InvalidParameters and aborts (or retries a request that can never succeed), and a caller matching AWS's fixed sentence 'The specified pool was not found.' never matches, because the emulator's message varies with every request ARN and so cannot be matched as a constant at all.
Fix: the DeleteTapePool handler's pool-lookup/not-found path in the Storage Gateway service, and the shared resource-lookup error constructor it calls - select the resource-specific not-found errorCode ('PoolNotFound') rather than the generic InvalidParameters, and emit the fixed InvalidGatewayRequestException message, carrying the pool identity in the `error` member instead of in the message.
Traces: storagegateway-tape-pool-lifecycle#task#9 (representative), storagegateway-tape-pool-resource-not-found-errors#task#1.

Related but NOT covering this: open issue 20261110T034016Z-storagegateway-storage-gateway-error-replies-use-non-aws-exception covers CreateTapePool validation wording, the SerializationException classification and the DescribeGatewayInformation gateway-not-found message; it carries no replay key for the DeleteTapePool clusters and does not describe the errorCode mis-selection, so this must be closed separately.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; /__admin/health answered 200 and the data listener accepts). Divergence cluster service=storagegateway, operation=DeleteTapePool, verdict=mismatch, expected_status 400 / actual_status 400, 2 steps:

  $.error.errorCode: expected 'PoolNotFound', actual 'InvalidParameters'
  $.message:         expected 'The specified pool was not found.'
                     actual   'The tape pool <poolArn> was not found.'
                     (the recorded run shows the emulator splicing the request's own PoolARN, of the form arn:aws:storagegateway:<region>:<account>:tapepool/<poolId>, into the text; that ARN is a per-run value, the defect is that ANY variable part appears at all)

  cluster members: storagegateway-tape-pool-lifecycle#task#9, storagegateway-tape-pool-resource-not-found-errors#task#1

Model basis: services/service-com-amazonaws-storagegateway-storagegateway-20130630/smithy-model - protocol awsJson1_1; DeleteTapePool input PoolARN (required, @length 50..500), errors InternalServerError (500) and InvalidGatewayRequestException (400) only; InvalidGatewayRequestException members message + error -> StorageGatewayError{errorCode: ErrorCode enum, errorDetails}; ErrorCode enum includes the generic 'InvalidParameters' alongside resource-specific not-found codes (TapeCartridgeNotFound, VolumeNotFound, SnapshotNotFound, GatewayNotFound, ...).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Representative: fixture storagegateway-tape-pool-lifecycle, step task#9 - create a tape pool, delete it, then POST to the Storage Gateway endpoint with X-Amz-Target StorageGateway_20130630.DeleteTapePool and the PoolARN of a pool that no longer exists.
3. Equivalent: fixture storagegateway-tape-pool-resource-not-found-errors, step task#1 - call DeleteTapePool with a well-formed PoolARN that was never created.
4. In both cases observe HTTP 400 whose $.error.errorCode is 'InvalidParameters' and whose $.message interpolates the PoolARN, instead of errorCode 'PoolNotFound' with the fixed message 'The specified pool was not found.'
