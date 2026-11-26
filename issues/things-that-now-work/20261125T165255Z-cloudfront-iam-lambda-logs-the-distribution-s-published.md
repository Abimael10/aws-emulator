Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502
Issue-Key: cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502|run#40|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Commit: 6e4695f2e2e0380b6e4530904193a77f767a96ef

# cloudfront__iam__lambda__logs: the distribution's published DomainName is a <id>.cloudfront.net host the emulator does not serve, so the ErrorCachingMinTTL behavior is never reached

> _Note: this issue was identified against the code base at commit `6e4695f2e2e0380b6e4530904193a77f767a96ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: a Deployed distribution over a Lambda function-URL origin announces DomainName as <id>.cloudfront.net, an address the emulator does not serve, and no per-distribution listener is published in its place, so the viewer request that would observe error caching never leaves the client. Failing check: cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502.

AWS: Smithy (cloudfront, 2020-05-31) documents Distribution.DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net", and GetDistribution reports Status "Deployed" only once that name answers viewer traffic. In AWS a viewer GET https://<DomainName>/<path> is answered by the edge; a CustomErrorResponse with ErrorCachingMinTTL causes the edge to hold an origin 502 for that many seconds (the second viewer request is served from the error cache without reaching the origin), while an origin 502 carrying Cache-Control: no-store is not held and the origin is re-contacted. Both legs are observable only through that address.

Emulator: the distribution reaches Deployed and publishes DomainName <id>.cloudfront.net (this run: d5ni6znxjb8do0.cloudfront.net), a host that is not one of the emulator's own hosts and that a customer of the emulator cannot reach. The viewer request is never issued: "simparity.published.PublishedUrlRejected: endpoint_url was given https://<id>.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". The host label and the loopback port differ per run; the requirement is only that the announced DomainName is an address the emulator serves.

Why: a client following AWS's documented flow — create the distribution, wait for Deployed, then fetch https://<DomainName>/<path> — gets no answer at all, so error caching (and with it the whole custom-error-response contract) cannot be used or tested, even though the control plane reports a healthy Deployed distribution.

Fix: the CloudFront distribution create/read serializer that mints DomainName, plus the viewer listener it should stand up — the announced DomainName must be an address the emulator serves, and that listener must serve viewer requests by relaying to the origin and applying the distribution's CustomErrorResponses / ErrorCachingMinTTL and the origin's Cache-Control.

Related open issue 20261125T050522Z-cloudfront-iam-lambda-logs-a-deployed-distribution-over.md describes the same shape for this service group as an edge that never answers, but does not carry this check's replay key.

Traces: cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502.

## Evidence
Expected (AWS): GET https://<DomainName>/<path> against the Deployed distribution returns the origin's 502 through the edge; a repeat within ErrorCachingMinTTL is served from the error cache (origin invocation count unchanged), while a no-store 502 is not cached (origin is re-invoked).

Actual (emulator): no viewer request is made — "simparity.published.PublishedUrlRejected: endpoint_url was given https://d5ni6znxjb8do0.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:44573/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". The id label and port are per-run values.

## Steps to Reproduce
1. Create a Lambda function with a function URL that answers 502 (plain, and a variant with Cache-Control: no-store).
2. CreateDistribution with that function URL as a custom origin and a CustomErrorResponse for 502 with ErrorCachingMinTTL set; poll GetDistribution until Status=Deployed.
3. Read Distribution.DomainName.
4. GET https://<DomainName>/<path> twice.
5. Observe that no listener exists for that host, so neither the cached nor the uncached error leg can be observed.
