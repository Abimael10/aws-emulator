Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudfront__iam__lambda.apigateway-lambda-origin-cache-key-query-string-tracks-age-and-ttl-expiry
Issue-Key: apigateway__cloudfront__iam__lambda.apigateway-lambda-origin-cache-key-query-string-tracks-age-and-ttl-expiry|_poll_for#14|AssertionError|raise AssertionError(f"timed out waiting for {description}; last observation was {last!r}")
Issue-Commit: 5af22e124f1b4edcbe644b4b43fbf6a6331dae9c

# apigateway__cloudfront__iam__lambda: a Deployed distribution over an API Gateway origin refuses connections at its published edge address, so no viewer request is ever served

> _Note: this issue was identified against the code base at commit `5af22e124f1b4edcbe644b4b43fbf6a6331dae9c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
One root cause, grounded in one failing behavioral check that the replay reports owned by no open issue and blocked by no unimplemented stub.

Cause 1 of 1: a distribution that CreateDistribution accepted and that GetDistribution reports as Status=Deployed answers nothing at the address its published DomainName resolves to — the TCP connection is actively refused — so every viewer request through the edge fails at the transport, before any cache behavior exists to observe.

AWS: the CloudFront data plane is not in the Smithy model, so this is recorded behavior only; the control plane at issue is cloudfront CreateDistribution/GetDistribution (Distribution$DomainName is "the domain name that corresponds to the distribution" and is the host viewers request). On AWS, once GetDistribution reports Status=Deployed, a GET at that domain on a path the default cache behavior covers is served: the first request for a cache key is a Miss forwarded to the origin and returns the origin's status and body with X-Cache: Miss from cloudfront, and an identical repeat within the origin's freshness lifetime is a Hit. Here the origin is a REST API stage reached through a usage-plan API key injected as an origin custom header, and the cache policy keys on the `v` query string. The requirement is that a Deployed distribution's published domain accepts connections and answers HTTP; which port or host the emulator chooses to serve it on is environment-specific and is not the requirement.

Emulator: the edge address derived from the distribution's own published DomainName refuses the connection on every attempt for 240 s — the check's bounded poll for the first 200 ends with `(None, {}, "URLError(ConnectionRefusedError(111, 'Connection refused'))")`, i.e. no HTTP response at all, no status and no headers. The same run proves the origin and the control plane are fine: the direct request at the REST API stage carrying no API key is answered 403 {"message": "Forbidden"} (that assertion passed), CreateDistribution returned an Id and DomainName, and the poll for Status=Deployed completed before the first edge request was attempted. So the distribution announces an edge address that nothing serves.

Why: a client that fronts an API Gateway stage with CloudFront can observe nothing through the distribution — not the cache key, not Miss/Hit, not Age, not TTL expiry. A connection refusal is also the worst failure mode for a caller: instead of an HTTP error it can classify, it gets a transport error, so retry logic and health checks loop or abort rather than report a CloudFront condition.

Fix: the CloudFront edge data plane's listener/addressing — the host and port a Deployed distribution's published DomainName resolves to must be an address the emulator accepts connections on and serves viewer requests for, with the request routed to the configured origin.

Traces: apigateway__cloudfront__iam__lambda.apigateway-lambda-origin-cache-key-query-string-tracks-age-and-ttl-expiry. The resolved issue 20261117T134237Z-apigateway-cloudfront-iam-lambda-a-cloudfront-cache-hit-carries-no-age-response.md is grounded in this very check and records the edge then answering 200 with `x-cache: Hit from cloudfront` (its subject was only the missing Age header); its subject is now unreachable because the edge answers nothing. Nothing is refused with an HTTP status or error code here — the connection itself is rejected — so this is filed as its own transport-level cause rather than described as a refusal regression of that issue.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (937/1144 passed; bootstrap_signal false; every service in this check's heading is bootstrapped). Per-service rate: apigateway__cloudfront__iam__lambda 0/1 (0%).

Failing check `apigateway__cloudfront__iam__lambda.apigateway-lambda-origin-cache-key-query-string-tracks-age-and-ttl-expiry`, verdict mismatch, detail verbatim:
  File "check_...apigateway_lambda_origin_cache_key_query_string_tracks_age_and_ttl_expiry.py", line 321, in run
    status, headers, body = _poll_for(lambda: fetch(v1), lambda r: r[0] == 200, timeout_s=240.0, ...)
  File "...", line 81, in _poll_for
    raise AssertionError(f"timed out waiting for {description}; last observation was {last!r}")
  AssertionError: timed out waiting for the first request to succeed; last observation was (None, {}, "URLError(ConnectionRefusedError(111, 'Connection refused'))")

Expected (AWS): GET <distribution-domain>/count?v=<value> -> HTTP 200 with the API Gateway Lambda-proxy body, `X-Cache: Miss from cloudfront` and the origin's `Cache-Control: max-age=60`; an identical repeat -> 200, `X-Cache: Hit from cloudfront`, same body.
Actual (emulator): no HTTP answer at all for 240 s; every probe ends in ConnectionRefusedError(111) at the address the distribution's DomainName resolves to.

Passing controls in the same check (not at issue): the direct stage request without an API key is answered 403 {"message": "Forbidden"}, and GetDistribution reaches Status=Deployed. The distribution id, domain, host, port and `v` value are per-run.

## Steps to Reproduce
1. Create a REST API with a Lambda-proxy integration answering `Cache-Control: max-age=60`, behind an API key in a usage plan, and deploy a stage.
2. Confirm a direct GET at the stage with no API key is 403 {"message": "Forbidden"} (this already works).
3. cloudfront CreateDistribution over that stage as a CustomOriginConfig origin, injecting the API key as an origin custom header, with a cache policy keyed on the `v` query string.
4. Poll GetDistribution until Status == 'Deployed' (this already completes).
5. GET <distribution DomainName>/count?v=<value>: AWS answers 200 with `X-Cache: Miss from cloudfront`; the emulator refuses the TCP connection (ConnectionRefusedError 111) and never answers within 240 s.
6. Re-replay apigateway__cloudfront__iam__lambda.apigateway-lambda-origin-cache-key-query-string-tracks-age-and-ttl-expiry and confirm it reaches its cache-key/Age/TTL assertions.
