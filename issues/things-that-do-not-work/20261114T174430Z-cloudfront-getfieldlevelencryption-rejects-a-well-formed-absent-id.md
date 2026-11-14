Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-not-found-error-paths
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-not-found-error-paths
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|GetFieldLevelEncryption|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: e2784ee3a9bae36394452385b38ba2b94553374b

# CloudFront GetFieldLevelEncryption rejects a well-formed absent id as malformed

> _Note: this issue was identified against the code base at commit `e2784ee3a9bae36394452385b38ba2b94553374b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetFieldLevelEncryption

## Description
One cause.

Cause 1 of 1: GetFieldLevelEncryption (GET /2020-05-31/field-level-encryption/{Id}) answers a lookup of an absent field-level-encryption config with the malformed-identifier refusal instead of the resource-not-found refusal.

AWS: 404 whose ErrorResponse.Error.Message is the fixed text 'The specified field level encryption config does not exist.' - recorded behavior only for the message; the Smithy model supplies the error shape but no text (GetFieldLevelEncryption: input Id is a required httpLabel targeting string with NO @pattern/@length constraint; http {method GET, uri /2020-05-31/field-level-encryption/{Id}, code 200}; errors AccessDenied httpError 403 and NoSuchFieldLevelEncryptionConfig httpError 404). Because the model puts no format constraint on Id, AWS carries the id through to a state lookup and reports the resource ABSENT; the message for this error class is entirely fixed text - it has no variable part, nothing in it is taken from the request or from emulator state - so reproducing it verbatim is the whole requirement. The status (404) and the error code already match.

Emulator: 404 with Error.Message 'Id format is invalid' - it applies an identifier-format check on this route and refuses the id as malformed before ever looking it up, so an id AWS accepts as well-formed-but-absent is reported as un-parseable.

Why: these are two different client contracts, and the emulator inverts them. 'Id format is invalid' tells a caller its own id handling is broken - a non-retryable programming error - while 'The specified field level encryption config does not exist.' tells it the resource is gone and the right response is to create or re-resolve it. A client that branches on the message (or a test asserting the not-found text) takes the wrong branch on every GetFieldLevelEncryption miss, and because the emulator's format predicate is narrower than AWS's (which has none, per the model), ids AWS would look up are never looked up at all.

Note the direction: this is the MIRROR of the open issue 20261114T142904Z-cloudfront-reopens-field-level-encryption-lookups-report-the.md, which covers GetFieldLevelEncryptionConfig, GetFieldLevelEncryptionProfile, DeleteFieldLevelEncryptionConfig and GetFieldLevelEncryptionProfileConfig reporting the resource absent when the id IS malformed. That issue does not carry this cluster's replay key and does not cover the GET /2020-05-31/field-level-encryption/{Id} route at all; its fix (add a format check before the lookup) would not fix this one, which needs the opposite correction on this route - the format predicate here must not reject ids the model places no constraint on.

Fix: GetFieldLevelEncryption's request validation / handler for GET /2020-05-31/field-level-encryption/{Id} - the path identifier must not be refused as malformed for a form AWS accepts; an id that is not present in state must fall through to the NoSuchFieldLevelEncryptionConfig 404 carrying the fixed not-found text.

Traces: cloudfront-not-found-error-paths#task#5 (1 affected step).

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false; 1303 steps, 1150 passed, score 88.3%). One unowned, unstubbed mismatch cluster (owned_by: [], stub_path: ""):

  divergence_key: service=cloudfront|GetFieldLevelEncryption|status=404->404|keys=$.ErrorResponse.Error.Message
  fixture_id: cloudfront-not-found-error-paths   step_key: task#5
  expected_status 404, actual_status 404 (status and Error.Code agree; only the message key diverges)
  detail: $.ErrorResponse.Error.Message: value_mismatch
    expected 'The specified field level encryption config does not exist.'
    actual   'Id format is invalid'
  affected_count: 1 (cloudfront-not-found-error-paths#task#5)

Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31): GetFieldLevelEncryption input {Id: string, httpLabel, required} with no pattern or length constraint; http GET /2020-05-31/field-level-encryption/{Id} code 200; errors AccessDenied (403), NoSuchFieldLevelEncryptionConfig (404). The expected message is 100% fixed text for this error class - no request value, state value, id, timestamp or request id appears in it.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout.
2. Replay the wire corpus simparity-wire-aws-v1.
3. Representative step: fixture cloudfront-not-found-error-paths, step task#5 - GET /2020-05-31/field-level-encryption/<an id in CloudFront's id form that names no existing FLE config>.
4. Observe the response: 404 with ErrorResponse.Error.Message 'Id format is invalid'. Expected: 404 with ErrorResponse.Error.Message 'The specified field level encryption config does not exist.'
5. The HTTP status (404) and the error code (NoSuchFieldLevelEncryptionConfig) already match - compare only Error.Message. Re-replay cloudfront-not-found-error-paths#task#5 to confirm a fix.
