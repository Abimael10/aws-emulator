Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__iam__lambda__logs.custom-error-response-rejects-a-negative-error-caching-min-ttl cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502
Issue-Key: cloudfront__iam__lambda__logs.custom-error-response-rejects-a-negative-error-caching-min-ttl|_expect_error#2|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502|run#50|AssertionError|assert warm_plain.content == cold_plain.content, (
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cloudfront__iam__lambda__logs: 2 causes — a negative CustomErrorResponse ErrorCachingMinTTL is accepted, and the configured ErrorCachingMinTTL does not hold a 502 at the edge

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Two net-new root causes in CloudFront's custom-error-response handling.

Cause 1 of 2: CreateDistribution accepts a negative ErrorCachingMinTTL.
AWS: Smithy cloudfront CustomErrorResponse$ErrorCachingMinTTL is 'the minimum amount of time, in seconds, that you want CloudFront to cache the HTTP status code specified in ErrorCode' — a duration, so a negative value is out of range and CreateDistribution refuses it with InvalidArgument 'The parameter ErrorCachingMinTTL is not within allowed range.' (fixed text; the parameter name is the only variable part).
Emulator: the same request is accepted — HTTP 201 Created with a Location and a full Distribution body, the negative TTL stored as configuration.
Why: a configuration AWS would never create is accepted, so an invalid template/IaC change passes in the emulator and fails in AWS, and the stored value then drives edge behavior no real distribution can have.
Fix: CreateDistribution/UpdateDistribution input validation for CustomErrorResponses (range check on ErrorCachingMinTTL, refused with InvalidArgument).
Failing check: cloudfront__iam__lambda__logs.custom-error-response-rejects-a-negative-error-caching-min-ttl

Cause 2 of 2: a configured ErrorCachingMinTTL does not cache an error response at the edge.
AWS: recorded behavior only for the edge — with a CustomErrorResponse for 502 and ErrorCachingMinTTL > 0, an immediate repeat of a request whose origin answered 502 WITHOUT a no-store/no-cache directive is served from the error cache (same body, origin not re-invoked); only an origin 502 that forbids storage is re-fetched.
Emulator: the immediate repeat returns a DIFFERENT body, i.e. the origin was invoked again although the plain 502 carried no Cache-Control header and the error TTL was configured.
Why: error caching is what shields an origin from a thundering herd during an outage; with it inert, every viewer retry reaches the origin, and the documented difference between a plain 502 and a no-store 502 is unobservable.
Fix: the edge's error-response caching stage — honour CustomErrorResponses' ErrorCachingMinTTL for an error the origin did not mark no-store.
Failing check: cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502

## Evidence
Cause 1: [mismatch] cloudfront__iam__lambda__logs.custom-error-response-rejects-a-negative-error-caching-min-ttl — AssertionError: 'expected the call to be rejected, but it succeeded: {... HTTPStatusCode 201 ... Distribution {...}}'; expected ClientError ('InvalidArgument', 'The parameter ErrorCachingMinTTL is not within allowed range.').

Cause 2: [mismatch] cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502 — AssertionError: 'an immediate repeat returned a different body (b'502-<id-a>' vs b'502-<id-b>') — the origin was re-invoked despite sending no Cache-Control header, which the configured Error Caching Minimum TTL should still hold' (the two marker bodies are per-run values; the requirement is that the repeat is served the cached body).

## Steps to Reproduce
1. CreateDistribution with CustomErrorResponses=[{ErrorCode: 502, ErrorCachingMinTTL: -1}] and observe a 201 instead of InvalidArgument.
2. CreateDistribution with a Lambda-backed origin that answers 502 with a fresh marker body and no Cache-Control, and a CustomErrorResponse for 502 with ErrorCachingMinTTL > 0; request the path twice back to back and compare the bodies (they must be identical), then repeat with the origin sending Cache-Control: no-store (the bodies must differ).
