Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502
Issue-Key: cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502|run#57|AssertionError|assert warm_nostore.content != cold_nostore.content, (
Issue-Commit: 145a098949592496b9e5a9b24629dbcb6ce40f89

# A 502 whose origin sends Cache-Control: no-store is cached at the edge, so an immediate repeat is served the stored error body instead of re-reaching the origin

> _Note: this issue was identified against the code base at commit `145a098949592496b9e5a9b24629dbcb6ce40f89`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
One net-new root cause in CloudFront's error-response caching at the edge. Related but distinct from open issue 20261117T115022Z-cloudfront-iam-lambda-logs-2-causes-a-negative.md, whose Cause 2 was the OPPOSITE symptom (a PLAIN 502 was not held by ErrorCachingMinTTL); that assertion now passes and the failure has moved to the no-store half of the same check, which the suite reports as not owned (its replay key is a different assertion).

Cause 1 of 1: the edge stores and replays an error response the origin forbade storing.
AWS: Smithy cloudfront CustomErrorResponse$ErrorCachingMinTTL is documented as "The minimum amount of time, in seconds, that you want CloudFront to cache the HTTP status code specified in ErrorCode. When this time period has elapsed, CloudFront queries your origin to see whether the problem that caused the error has been resolved" — it is the MINIMUM caching duration for an error the origin did not forbid caching, not an override of the origin's cache directives. Recorded behavior only for the edge data plane: with a CustomErrorResponse for 502 (ErrorCachingMinTTL > 0, no ResponsePagePath/ResponseCode, so the origin's status and body pass through) and a cache policy whose MinTTL/DefaultTTL are 0, an origin 502 carrying `Cache-Control: no-store` is NOT stored — each viewer request reaches the origin again and therefore observes a fresh origin-generated body — while an origin 502 carrying no Cache-Control header IS held for ErrorCachingMinTTL seconds.
Emulator: the two back-to-back requests to the no-store path return byte-identical bodies, i.e. the second request was answered from the edge's error cache and the origin was not re-invoked. The HTTP status is 502 on both (correct); the defect is that the stored entry exists at all.
Why: a client relying on AWS's behavior loses the one documented escape hatch an origin has for an error it must not have replayed. An origin that marks a transient failure `no-store` (a per-viewer authorization failure, a failure whose body carries request-specific detail, a failure it expects to clear immediately) has that response pinned at the edge for the whole ErrorCachingMinTTL window in the emulator: viewers are served a stale error after the origin has recovered, and an error body computed for one viewer is replayed to others. It also makes the documented distinction between a plain error and a no-store error unobservable, so a test that asserts an origin's no-store directive is respected passes in the emulator against a cache that ignores it.
Fix: the edge data plane's error-response caching stage — before storing a response selected by CustomErrorResponses, parse the origin response's Cache-Control (and Pragma) directives and skip storing when the origin forbids storage (no-store, and no-cache/`max-age=0`/`s-maxage=0` for revalidation), applying ErrorCachingMinTTL only as a floor for an error the origin left cacheable. ErrorCachingMinTTL must not be treated as an unconditional store.
Traces: cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502

## Evidence
[mismatch] cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502

AssertionError raised at the check's own assertion:
  assert warm_nostore.content != cold_nostore.content
  "an immediate repeat returned the SAME body (b'502-<nonce>') — a 502 whose origin sends Cache-Control: no-store must not be cached, so each request must reach the origin again"

Expected (AWS): GET <edge>/err-502-no-store twice back to back -> both responses 502 with a body of the form `502-<fresh nonce>`, and the two bodies DIFFER, because the origin was invoked for each request.
Actual (emulator): both responses 502, and the two bodies are IDENTICAL — the second was served from the edge error cache.

The nonce in the body is a per-run value minted by the origin function (uuid4 hex); it is not part of the requirement. The requirement is only that two consecutive responses to a no-store 502 are produced by two separate origin invocations and therefore are not byte-identical, while the plain-502 path (asserted earlier in the same check and now passing) must be byte-identical for ErrorCachingMinTTL seconds.

No error code or error message is involved: both responses carry the origin's own 502 status, which is correct.

## Steps to Reproduce
1. Create a Lambda function whose handler returns a fresh nonce in its body and, for path /err-502-no-store, status 502 with header `Cache-Control: no-store`; for path /err-502-plain, status 502 with no Cache-Control header. Create a function URL for it (AWS_IAM auth).
2. CreateCachePolicy with MinTTL=0, DefaultTTL=0, MaxTTL=30 and no headers/cookies/query strings in the cache key.
3. CreateOriginAccessControl with OriginAccessControlOriginType=lambda, SigningBehavior=always, SigningProtocol=sigv4.
4. CreateDistribution with that function URL host as a custom origin (https-only) using the cache policy, the managed `Managed-AllViewerExceptHostHeader` origin request policy and the OAC, plus CustomErrorResponses={Quantity:1, Items:[{ErrorCode:502, ErrorCachingMinTTL:10}]} and no ResponsePagePath/ResponseCode. Grant lambda:InvokeFunctionUrl / lambda:InvokeFunction to cloudfront.amazonaws.com for the distribution ARN. Wait for Status=Deployed.
5. GET <domain>/err-502-plain twice back to back: both must be 502 with the SAME body (error caching holds).
6. GET <domain>/err-502-no-store twice back to back: both must be 502 with DIFFERENT bodies. The emulator returns the same body on the second request — the no-store 502 was cached.
