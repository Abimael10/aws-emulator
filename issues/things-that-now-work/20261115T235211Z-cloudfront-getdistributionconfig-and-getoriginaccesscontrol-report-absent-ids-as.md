Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-distribution-not-found-error-paths
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-distribution-not-found-error-paths cloudfront-not-found-error-paths cloudfront-oac-error-paths
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|GetDistributionConfig|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|GetOriginAccessControl|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: b8a6145c82ff7150299d1bdeea75ea37edcc92b2

# CloudFront GetDistributionConfig and GetOriginAccessControl report absent ids as malformed

> _Note: this issue was identified against the code base at commit `b8a6145c82ff7150299d1bdeea75ea37edcc92b2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDistributionConfig

## Description
Two causes. Both are the same shared fault on two different read routes: an identifier-format predicate is applied to the path label BEFORE the state lookup, so an id AWS accepts as well-formed-but-absent is refused with a malformed-identifier message instead of the resource-not-found message modelled for that route. The HTTP status (404) and the error code already match in both; only ErrorResponse.Error.Message diverges.

Cause 1 of 2: GetDistributionConfig (GET /2020-05-31/distribution/{Id}/config) — fixture cloudfront-distribution-not-found-error-paths, step task#1.
AWS: 404 whose ErrorResponse.Error.Message is the fixed text 'The specified distribution does not exist.' — Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31): GetDistributionConfig input {Id: string, @httpLabel, required} with NO @pattern and NO @length constraint; http {method GET, uri /2020-05-31/distribution/{Id}/config, code 200}; errors AccessDenied (httpError 403) and NoSuchDistribution (httpError 404). The model places no format constraint on Id, so AWS carries whatever id it is given through to the state lookup and reports the resource ABSENT. The message for this error class is entirely fixed text — it has no variable part, nothing in it is taken from the request or from emulator state — so reproducing it verbatim is the whole requirement (message text per recorded behavior; the model supplies the error shape but no wording).
Emulator: 404 with ErrorResponse.Error.Message 'Id format is invalid' — it runs an id-shape check on this route and refuses the label as un-parseable before ever consulting state.
Why: the two messages are different client contracts and the emulator inverts them. 'Id format is invalid' tells a caller its own id handling is broken (a non-retryable programming error); 'The specified distribution does not exist.' tells it the distribution is gone and the right response is to create or re-resolve it. A client (or a test) that branches on the not-found wording takes the wrong branch on every GetDistributionConfig miss, and because the emulator's format predicate is narrower than AWS's (which has none, per the model), ids AWS would look up are never looked up at all.
Fix: GetDistributionConfig's request validation / handler for GET /2020-05-31/distribution/{Id}/config — the path identifier must not be refused as malformed for a form the model places no constraint on; an id absent from state must fall through to the NoSuchDistribution 404 carrying the fixed not-found text.
Traces: cloudfront-distribution-not-found-error-paths#task#1 (1 affected step).

Cause 2 of 2: GetOriginAccessControl (GET /2020-05-31/origin-access-control/{Id}) — fixture cloudfront-not-found-error-paths, step task#6.
AWS: 404 whose ErrorResponse.Error.Message is the fixed text 'The specified origin access control does not exist.' — Smithy: GetOriginAccessControl input {Id: string, @httpLabel, required} with NO @pattern and NO @length constraint; http {method GET, uri /2020-05-31/origin-access-control/{Id}, code 200}; errors AccessDenied (403) and NoSuchOriginAccessControl (404). Again no format constraint on Id, so the id reaches the lookup and the resource is reported absent; the message is 100% fixed text for this error class (message text per recorded behavior).
Emulator: 404 with ErrorResponse.Error.Message 'Id format is invalid' — the same pre-lookup format refusal on the origin-access-control read route.
Why: same inversion — a caller probing for an OAC that is simply not there is told its id is unusable, so retry/create logic that keys on the not-found text never runs, and well-formed ids outside the emulator's narrower id shape are never resolved against state at all.
Fix: GetOriginAccessControl's request validation / handler for GET /2020-05-31/origin-access-control/{Id} — drop the pre-lookup format rejection for forms the model does not constrain and emit NoSuchOriginAccessControl 404 with the fixed not-found text for an id that names no OAC.
Traces: cloudfront-not-found-error-paths#task#6, cloudfront-oac-error-paths#task#3 (2 affected steps).

Related but NOT covering: open issue 20261114T174430Z-cloudfront-getfieldlevelencryption-rejects-a-well-formed-absent-id.md describes the identical inversion on GET /2020-05-31/field-level-encryption/{Id}, and 20261114T142904Z-cloudfront-reopens-field-level-encryption-lookups-report-the.md describes the mirror direction on the field-level-encryption config/profile routes. Neither carries these clusters' replay keys and neither touches the distribution-config or origin-access-control routes, so fixing either would not fix these two causes.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false; 1303 steps, 1165 passed, score 89.4%, 57 divergence clusters). These are the only two clusters that are unowned and unstubbed (owned_by: [], stub_path: "").

Cause 1 cluster:
  divergence_key: service=cloudfront|GetDistributionConfig|status=404->404|keys=$.ErrorResponse.Error.Message
  fixture_id: cloudfront-distribution-not-found-error-paths   step_key: task#1
  expected_status 404, actual_status 404 (status and Error.Code agree; only the message key diverges)
  detail: $.ErrorResponse.Error.Message: value_mismatch
    expected 'The specified distribution does not exist.'
    actual   'Id format is invalid'
  affected_count: 1 (cloudfront-distribution-not-found-error-paths#task#1)

Cause 2 cluster:
  divergence_key: service=cloudfront|GetOriginAccessControl|status=404->404|keys=$.ErrorResponse.Error.Message
  fixture_id: cloudfront-not-found-error-paths   step_key: task#6
  expected_status 404, actual_status 404 (status and Error.Code agree; only the message key diverges)
  detail: $.ErrorResponse.Error.Message: value_mismatch
    expected 'The specified origin access control does not exist.'
    actual   'Id format is invalid'
  affected_count: 2 (cloudfront-not-found-error-paths#task#6, cloudfront-oac-error-paths#task#3)

Message anatomy for both: the expected text is entirely FIXED for its error class — no request value, state value, id, timestamp or request id appears in it — so no part of it may be templated from a recording. The emulator's 'Id format is invalid' is likewise fixed text, from the wrong error class.

Smithy evidence (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json): GetDistributionConfig — Id @httpLabel required string, no pattern/length; errors AccessDenied 403, NoSuchDistribution 404. GetOriginAccessControl — Id @httpLabel required string, no pattern/length; errors AccessDenied 403, NoSuchOriginAccessControl 404. Neither input constrains the identifier's form, so neither route has a modelled basis for a malformed-identifier refusal.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout.
2. Replay the wire corpus simparity-wire-aws-v1.
3. Cause 1 — representative step: fixture cloudfront-distribution-not-found-error-paths, step task#1: GET /2020-05-31/distribution/<a well-formed CloudFront distribution id that names no existing distribution>/config. Observe 404 with ErrorResponse.Error.Message 'Id format is invalid'; expected 404 with 'The specified distribution does not exist.' (code NoSuchDistribution, status 404 already match — compare only Error.Message).
4. Cause 2 — representative step: fixture cloudfront-not-found-error-paths, step task#6: GET /2020-05-31/origin-access-control/<a well-formed OAC id that names no existing origin access control>. Observe 404 with ErrorResponse.Error.Message 'Id format is invalid'; expected 404 with 'The specified origin access control does not exist.' (code NoSuchOriginAccessControl, status 404 already match).
5. Re-replay cloudfront-distribution-not-found-error-paths#task#1 AND cloudfront-not-found-error-paths#task#6 (plus cloudfront-oac-error-paths#task#3) to confirm BOTH causes are closed before submitting.
