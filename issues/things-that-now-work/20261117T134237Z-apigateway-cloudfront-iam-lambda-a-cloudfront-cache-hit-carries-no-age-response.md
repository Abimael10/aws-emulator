Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudfront__iam__lambda.apigateway-lambda-origin-cache-key-query-string-tracks-age-and-ttl-expiry
Issue-Key: apigateway__cloudfront__iam__lambda.apigateway-lambda-origin-cache-key-query-string-tracks-age-and-ttl-expiry|_poll_for#22|AssertionError|raise AssertionError(f"timed out waiting for {description}; last observation was {last!r}")
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# A CloudFront cache Hit carries no Age response header, so a cached object's age never advances

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: a response CloudFront serves from its cache omits the `Age` header entirely.

AWS: recorded behavior only for the CloudFront data plane (it is not in the Smithy model; the control plane is cloudfront CreateDistribution/GetDistribution). When CloudFront serves a response from an edge cache it sets `X-Cache: Hit from cloudfront` and, as RFC 9111 requires of every cache, an `Age` response header carrying the number of seconds the object has been held — so two Hits on the same cached object separated in time report a strictly growing `Age`, and an object held past its `Cache-Control: max-age` is revalidated and becomes a `Miss from cloudfront` again.

Emulator: the Hit is served correctly as far as `x-cache` and the body go — the response carries `x-cache: 'Hit from cloudfront'`, `cache-control: 'max-age=60'` and the first Miss's body — but there is no `age` header among the response headers at all. The check's bounded poll for a Hit with `Age >= 1` therefore never succeeds and reports `timed out waiting for a Hit of the first Miss's object with Age >= 1`, with the last observation showing the full header set and no `age` member.

Why: a client cannot tell how stale a cached response is. Any caller that reads `Age` — to compute remaining freshness, to decide whether to revalidate, or to assert that a cache is actually holding an object across requests — gets nothing, and cache-behavior tests that distinguish a fresh Hit from an aged one cannot be written against the emulator.

Fix: the CloudFront data-plane response serializer for a cache Hit — it must emit `Age: <seconds the entry has been held>`, derived from the cache entry's insertion time, alongside the `X-Cache` header it already sets.

Traces: apigateway__cloudfront__iam__lambda.apigateway-lambda-origin-cache-key-query-string-tracks-age-and-ttl-expiry

## Evidence
Expected: a cache Hit carries `age` >= 1, and a later Hit on the same object carries a strictly larger `age`.
Actual, verbatim:
  AssertionError: timed out waiting for a Hit of the first Miss's object with Age >= 1; last observation was (200, {'x-cache': 'Hit from cloudfront', 'x-amz-cf-id': '...', 'cache-control': 'max-age=60', 'x-amzn-requestid': '...', 'content-type': 'application/json', 'content-length': '78', 'connection': 'close', 'date': '...'}, '{"request_id": "...", "v": "v1-..."}')
The header map is complete and contains no `age` key at all — the status (200), `x-cache` value and body are right; the `Age` header is simply never emitted. (The cf-id, request id, date and body values are per-run.)

## Steps to Reproduce
1. Create a REST API Lambda-proxy origin whose integration answers `Cache-Control: max-age=60`, behind an API key in a usage plan.
2. cloudfront CreateDistribution over that origin with a cache policy keyed on the `v` query string; wait for Deployed.
3. GET `/count?v=<value>` once (Miss), then GET the same URL again a few seconds later.
4. Observe the second response: `x-cache: Hit from cloudfront` but no `age` header. Expected an `age` header >= 1 on the Hit, and a strictly larger `age` on a later Hit of the same object.
