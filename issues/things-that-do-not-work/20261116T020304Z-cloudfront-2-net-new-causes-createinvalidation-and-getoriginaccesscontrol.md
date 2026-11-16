Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.op-contract-createInvalidation cloudfront.op-contract-get-origin-access-control
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront.op-contract-createInvalidation|run#17|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: cloudfront.op-contract-get-origin-access-control|run#14|AssertionError|assert message == "The specified origin access control does not exist.", (
Issue-Commit: 53c167456b2a6d63c5ef974788af7a32439016fd

# cloudfront: 2 net-new causes — CreateInvalidation and GetOriginAccessControl report a well-formed absent id as a malformed id instead of using AWS's not-found message

> _Note: this issue was identified against the code base at commit `53c167456b2a6d63c5ef974788af7a32439016fd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateInvalidation

## Description
Two net-new root causes in the CloudFront control plane. Both are the same shape of defect on two different routes: the handler runs an identifier-format predicate on the request's path label and, when the id is not found, emits a malformed-identifier message instead of the fixed not-found message AWS words for that error class. In both cases the HTTP status and the error code already agree with AWS; only the error Message diverges, so a client that branches on the wording takes the wrong branch.

Cause 1 of 2: CreateInvalidation against a distribution id that does not exist (failing check: cloudfront.op-contract-createInvalidation).
AWS: 404 with error code NoSuchDistribution and Message 'The specified distribution does not exist.' — Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31): CreateInvalidation input {DistributionId: string, @httpLabel, required} with NO @pattern and NO @length constraint, InvalidationBatch @httpPayload required; http {method POST, uri /2020-05-31/distribution/{DistributionId}/invalidation, code 201}; errors AccessDenied (403), BatchTooLarge (413), InconsistentQuantities (400), InvalidArgument (400), MissingBody (400), NoSuchDistribution (404), TooManyInvalidationsInProgress (400). Because the model places no format constraint on DistributionId, AWS carries any supplied id through to the state lookup and reports the distribution ABSENT. The message for this class is entirely FIXED text — no part of it is taken from the request or from emulator state (no id, name, number or timestamp) — so reproducing it verbatim is the whole requirement (message wording per recorded behavior; the model supplies the error shape, not the prose).
Emulator: answers the same 404/NoSuchDistribution but with Message 'DistributionId format is invalid' — a fixed string from the wrong error class, produced by a pre-lookup id-shape check on the invalidation route.
Why: 'DistributionId format is invalid' tells a caller its own id handling is broken (a non-retryable programming error), whereas 'The specified distribution does not exist.' tells it the distribution is gone and the correct response is to re-resolve or create it. Invalidation drivers, CI scripts and SDK wrappers that key on the not-found wording never take their create/re-resolve branch, and because the emulator's format predicate is narrower than AWS's (which has none per the model), ids AWS would resolve are never looked up against state at all.
Fix: CreateInvalidation's request validation / handler for POST /2020-05-31/distribution/{DistributionId}/invalidation — do not refuse the path identifier as malformed for a form the model places no constraint on, and emit the NoSuchDistribution 404 carrying its fixed not-found text when the id names no distribution.
Traces: failing check cloudfront.op-contract-createInvalidation (the check's happy path — echoed batch, InProgress status, Location, GetInvalidation readback — passed; only the not-found message assertion failed).

Cause 2 of 2: GetOriginAccessControl for an origin access control id that does not exist (failing check: cloudfront.op-contract-get-origin-access-control).
AWS: 404 with error code NoSuchOriginAccessControl and Message 'The specified origin access control does not exist.' — Smithy: GetOriginAccessControl input {Id: string, @httpLabel, required} with NO @pattern and NO @length constraint; http {method GET, uri /2020-05-31/origin-access-control/{Id}, code 200}; errors AccessDenied (403), NoSuchOriginAccessControl (404). Again the id is unconstrained by the model, so it reaches the lookup and the resource is reported absent; this message too is 100% fixed text for its error class (wording per recorded behavior).
Emulator: answers 404/NoSuchOriginAccessControl with Message 'Id format is invalid' — the same pre-lookup format refusal, on the origin-access-control read route.
Why: a caller probing for an OAC that is simply not there is told its id is unusable, so create-if-absent and retry logic that keys on the not-found text never runs; cleanup paths that swallow not-found by message also stop recognising it.
Fix: GetOriginAccessControl's request validation / handler for GET /2020-05-31/origin-access-control/{Id} — drop the pre-lookup malformed-id refusal for forms the model does not constrain and emit NoSuchOriginAccessControl 404 with its fixed not-found text.
Traces: failing check cloudfront.op-contract-get-origin-access-control (create + readback + ETag assertions passed; only the unknown-id message assertion failed).

Related but NOT covering: open issue 20261115T235211Z-cloudfront-getdistributionconfig-and-getoriginaccesscontrol-report-absent-ids-as.md describes the same inversion on GetDistributionConfig and GetOriginAccessControl from the WIRE corpus, but it carries wire replay keys only and does not carry these behavioral checks' replay keys, and it does not touch CreateInvalidation at all. Open issues 20261114T174430Z-cloudfront-getfieldlevelencryption-rejects-a-well-formed-absent-id.md, 20261114T142904Z-cloudfront-reopens-field-level-encryption-lookups-report-the.md and 20261115T095648Z-cloudfront-accept-refuse-decisions-diverge-anycast-quota-if.md describe the same family on field-level-encryption, anycast and invalidation-id-format routes; none of them cover these two routes. Fixing any of them would leave both causes above open.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (495/632 passed, 78.3%; bootstrap_signal false — the emulator served these calls and answered them wrongly).

Cause 1 — check cloudfront.op-contract-createInvalidation, verdict mismatch:
  cf.create_invalidation(DistributionId='EDFAKEDOESNOTEXIST1', InvalidationBatch={CallerReference: <unique>, Paths: {Quantity: 1, Items: ['/*']}})
  assertion: `assert message == expected` ->
  AssertionError: expected message 'The specified distribution does not exist.', got 'DistributionId format is invalid'
  The preceding `assert code == 'NoSuchDistribution'` passed, so the error code and the 404 status already match; ONLY Error.Message diverges.
  Message anatomy: expected text is wholly fixed for the NoSuchDistribution class on this route — it contains no request value, id, limit, timestamp or request id, so nothing in it may be templated from a recording. The emulator's 'DistributionId format is invalid' is likewise fixed text, but belongs to a malformed-argument class AWS does not use here.

Cause 2 — check cloudfront.op-contract-get-origin-access-control, verdict mismatch:
  cf.get_origin_access_control(Id='OACBOGUS0000000000')
  assertion: `assert message == 'The specified origin access control does not exist.'` ->
  AssertionError: expected AWS's exact not-found message, got 'Id format is invalid'
  The preceding `assert err == 'NoSuchOriginAccessControl'` passed, so code and 404 status match; ONLY Error.Message diverges. Expected text is entirely fixed for the NoSuchOriginAccessControl class; the emulator's 'Id format is invalid' is fixed text from the wrong class.

Smithy evidence (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json): CreateInvalidation — DistributionId @httpLabel required string, no pattern/length; errors include NoSuchDistribution httpError 404 and InvalidArgument httpError 400 (a distinct class, not the one AWS selects for an absent distribution). GetOriginAccessControl — Id @httpLabel required string, no pattern/length; errors AccessDenied 403, NoSuchOriginAccessControl 404. Neither input constrains the identifier's form, so neither route has a modelled basis for a malformed-identifier refusal.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout.
2. Replay the behavioral suite simparity-behavioral-aws-v1.
3. Cause 1 — check cloudfront.op-contract-createInvalidation: create a minimal distribution (so the happy path passes), then call CreateInvalidation with DistributionId set to a well-formed CloudFront distribution id that names no existing distribution (e.g. 'EDFAKEDOESNOTEXIST1') and any valid InvalidationBatch. Observe 404 NoSuchDistribution with Error.Message 'DistributionId format is invalid'; expected 404 NoSuchDistribution with Error.Message 'The specified distribution does not exist.' (compare ONLY Error.Message — status and code already match).
4. Cause 2 — check cloudfront.op-contract-get-origin-access-control: create an origin access control (happy path passes), then call GetOriginAccessControl with a well-formed id that names no OAC (e.g. 'OACBOGUS0000000000'). Observe 404 NoSuchOriginAccessControl with Error.Message 'Id format is invalid'; expected 404 NoSuchOriginAccessControl with Error.Message 'The specified origin access control does not exist.'
5. Re-replay BOTH cloudfront.op-contract-createInvalidation AND cloudfront.op-contract-get-origin-access-control to confirm both causes are closed before submitting.
