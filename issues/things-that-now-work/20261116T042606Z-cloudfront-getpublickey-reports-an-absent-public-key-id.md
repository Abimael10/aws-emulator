Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-public-key-duplicate-and-notfound-errors
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-public-key-duplicate-and-notfound-errors cloudfront-public-key-error-paths
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|GetPublicKey|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 76d00de73023d709f64dd9c70d1a3cce4abd8214

# CloudFront GetPublicKey reports an absent public key id as malformed

> _Note: this issue was identified against the code base at commit `76d00de73023d709f64dd9c70d1a3cce4abd8214`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetPublicKey

## Description
One cause.

Cause 1 of 1: GetPublicKey (GET /2020-05-31/public-key/{Id}) refuses a well-formed-but-absent public key id with a malformed-identifier message instead of the modelled resource-not-found message. Fixture cloudfront-public-key-duplicate-and-notfound-errors, step task#3.

AWS: 404 whose ErrorResponse.Error.Message is the fixed text 'The specified CloudFront public key does not exist.' Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json): GetPublicKey input {Id: string, @httpLabel, required} carries NO @pattern and NO @length constraint; http {method GET, uri /2020-05-31/public-key/{Id}, code 200}; errors AccessDenied (httpError 403) and NoSuchPublicKey (httpError 404), documented as 'The specified public key doesn't exist.' Because the model places no format constraint on Id, AWS carries whatever identifier it is given through to the state lookup and reports the resource ABSENT rather than the identifier unusable. Message anatomy: the wire text for this error class is ENTIRELY FIXED — it contains no id, name, value, timestamp or request id taken from the request or from emulator state — so reproducing it verbatim is the whole requirement (exact wording per recorded behavior; the model supplies the error shape and status but not the wording).

Emulator: 404 with ErrorResponse.Error.Message 'Id format is invalid'. An identifier-format predicate is evaluated against the path label BEFORE the state lookup, so the request never reaches the public-key store. The HTTP status (404) and Error.Code already agree with AWS; only ErrorResponse.Error.Message diverges.

Why: the two messages are different client contracts and the emulator inverts them. 'Id format is invalid' tells a caller its own identifier handling is broken — a non-retryable programming error. 'The specified CloudFront public key does not exist.' tells the caller the key is simply absent, so the correct response is to create it or re-resolve it. A client or test that branches on the not-found wording takes the wrong branch on every GetPublicKey miss. Worse, the emulator's format predicate is strictly narrower than AWS's (which has none, per the model), so any public key id outside the emulator's assumed shape is never resolved against state at all, even when the key exists.

Fix: GetPublicKey's request validation / handler for GET /2020-05-31/public-key/{Id} — drop the pre-lookup identifier-format rejection for a form the model does not constrain, and let an id that names no stored public key fall through to the NoSuchPublicKey 404 carrying the fixed not-found text.

Traces: cloudfront-public-key-duplicate-and-notfound-errors#task#3 (representative) and cloudfront-public-key-error-paths#task#2 (2 affected steps in the cluster).

Related but NOT covering: open issue 20261115T235211Z-cloudfront-getdistributionconfig-and-getoriginaccesscontrol-report-absent-ids-as.md describes the identical pre-lookup-format inversion on GET /2020-05-31/distribution/{Id}/config and GET /2020-05-31/origin-access-control/{Id}. It carries neither this cluster's replay key nor the public-key route, so closing it would not close this cause.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false; 1303 steps, 1169 passed, 129 mismatched, 5 blocked, score 89.7%, 55 divergence clusters). This is the only cluster that is unowned and unstubbed (owned_by: [], stub_path: ""); the other 54 clusters are already owned by open issues that carry their replay keys.

Cluster:
  divergence_key: service=cloudfront|GetPublicKey|status=404->404|keys=$.ErrorResponse.Error.Message
  fixture_id: cloudfront-public-key-duplicate-and-notfound-errors   step_key: task#3
  verdict: mismatch
  expected_status 404, actual_status 404 (status and Error.Code agree; only the message key diverges)
  detail: $.ErrorResponse.Error.Message: value_mismatch
    expected 'The specified CloudFront public key does not exist.'
    actual   'Id format is invalid'
  affected_count: 2 (cloudfront-public-key-duplicate-and-notfound-errors#task#3, cloudfront-public-key-error-paths#task#2)

Message anatomy: the expected text is fixed for its error class — no request value, state value, id, timestamp or request id appears in it, so no part of it may be templated from a single recording. The emulator's 'Id format is invalid' is likewise fixed text, but from the wrong error class (a malformed-input refusal rather than a resource-absent refusal).

Smithy evidence: GetPublicKey — Id @httpLabel required string, no pattern/length; output {PublicKey @httpPayload, ETag @httpHeader}; errors AccessDenied 403, NoSuchPublicKey 404 ('The specified public key doesn't exist.'). The input places no constraint on the identifier's form, so this route has no modelled basis for a malformed-identifier refusal.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout.
2. Replay the wire corpus simparity-wire-aws-v1.
3. Representative step: fixture cloudfront-public-key-duplicate-and-notfound-errors, step task#3 — GET /2020-05-31/public-key/<a well-formed CloudFront public key id that names no existing public key>.
4. Observe 404 with ErrorResponse.Error.Message 'Id format is invalid'. Expected 404 with ErrorResponse.Error.Message 'The specified CloudFront public key does not exist.' (status 404 and Error.Code NoSuchPublicKey already match — compare only Error.Message).
5. Re-replay cloudfront-public-key-duplicate-and-notfound-errors#task#3 and cloudfront-public-key-error-paths#task#2 to confirm the cause is closed before submitting.
