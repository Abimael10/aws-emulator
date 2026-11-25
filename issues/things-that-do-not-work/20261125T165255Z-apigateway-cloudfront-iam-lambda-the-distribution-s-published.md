Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudfront__iam__lambda.apigateway-lambda-origin-cache-key-query-string-tracks-age-and-ttl-expiry
Issue-Key: apigateway__cloudfront__iam__lambda.apigateway-lambda-origin-cache-key-query-string-tracks-age-and-ttl-expiry|run#8|PublishedUrlRejected|base, route = ctx.endpoint_route(dist["domain"])
Issue-Commit: 6e4695f2e2e0380b6e4530904193a77f767a96ef

# apigateway__cloudfront__iam__lambda: the distribution's published DomainName is a <id>.cloudfront.net host the emulator does not serve, so the API-Gateway-origin cache-key/TTL check never reaches the edge

> _Note: this issue was identified against the code base at commit `6e4695f2e2e0380b6e4530904193a77f767a96ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: a Deployed distribution over an API Gateway + Lambda origin announces DomainName as <id>.cloudfront.net, an address the emulator does not serve, and no per-distribution listener is published in its place, so no viewer request can reach the edge and the cache-key / Age / TTL-expiry behavior is never exercised. Failing check: apigateway__cloudfront__iam__lambda.apigateway-lambda-origin-cache-key-query-string-tracks-age-and-ttl-expiry.

AWS: Smithy (cloudfront, 2020-05-31) documents Distribution.DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net", and GetDistribution reports Status "Deployed" only once that name answers viewer traffic. In AWS a viewer GET https://<DomainName>/<path>?<query> is answered by the edge, which relays to the configured origin and returns the origin's body together with the edge's cache headers (X-Cache Hit/Miss from cloudfront, Age, Via). The cache-key contribution of the query string and the expiry of a cached entry at its TTL are observable only through that address.

Emulator: the distribution reaches Deployed and publishes DomainName <id>.cloudfront.net (this run: dvbb9jkpe6kwe0.cloudfront.net). That host is not one of the emulator's own hosts and nothing is published that a customer of the emulator could reach, so the viewer request is never issued: "simparity.published.PublishedUrlRejected: endpoint_url was given https://<id>.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". The check dies there, before any cache-key or TTL assertion runs. The host label and the loopback port differ per run; the requirement is only that the announced DomainName is an address the emulator serves.

Why: a client that does exactly what AWS documents — CreateDistribution, poll until Deployed, then GET https://<DomainName>/<path> — gets no answer at all. The CloudFront data plane for an API Gateway origin is unusable: query-string cache keys, Age accounting and TTL expiry cannot be used or tested against the emulator even though the control plane reports a healthy Deployed distribution.

Fix: the CloudFront distribution create/read serializer that mints DomainName, plus the viewer listener it should stand up — the announced DomainName must be an address the emulator actually serves (its own host/port, or a per-distribution listener), and that listener must serve viewer requests by relaying to the configured origin and applying the attached cache policy.

Related open issue 20261125T050522Z-apigateway-cloudfront-iam-lambda-a-deployed-distribution-over.md describes the same shape for this service group as a connection refusal, but does not carry this check's replay key.

Traces: apigateway__cloudfront__iam__lambda.apigateway-lambda-origin-cache-key-query-string-tracks-age-and-ttl-expiry.

## Evidence
Expected (AWS): GET https://<DomainName>/<path>?<query> on a Deployed distribution returns 200 with the API Gateway/Lambda origin's body and the edge's cache headers, so the query-string cache-key, Age and TTL-expiry assertions can run.

Actual (emulator): the request is never issued — the published endpoint is rejected as unserveable: "simparity.published.PublishedUrlRejected: endpoint_url was given https://dvbb9jkpe6kwe0.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:45195/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". The distribution id label and the port are per-run values; the fixed content is that the announced host is not served.

## Steps to Reproduce
1. Create a REST API with a Lambda proxy integration and deploy it to a stage.
2. CreateDistribution with that stage as a custom origin and a cache policy whose cache key includes the query string; poll GetDistribution until Status=Deployed.
3. Read Distribution.DomainName from the create/read response.
4. GET https://<DomainName>/<path>?k=v.
5. Observe no listener exists for that host — the viewer request cannot be issued, while AWS answers it from the edge.
