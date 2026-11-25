Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-brotli-enabled-cache-policy-serves-br-and-keys-on-encoding cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-cache-hit-leaves-origin-invocation-count-unchanged cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-edge-generates-viewer-country-and-forwarded-proto cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-path-prefixes-the-path-the-origin-sees cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-query-string-allexcept-and-all-shape-the-cache-key cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-request-policy-forwards-what-the-cache-key-excludes cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-unlisted-header-cookie-query-blocked-from-origin cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-viewer-spoofed-edge-headers-rejected-at-the-edge
Issue-Key: cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-brotli-enabled-cache-policy-serves-br-and-keys-on-encoding|_warm_edge#1|AssertionError|raise AssertionError(f"{DATAPLANE_MARKER}: {exc} (last {last.get('v')!r})") from exc
Issue-Key: cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-cache-hit-leaves-origin-invocation-count-unchanged|_warm_edge#1|AssertionError|raise AssertionError(f"{DATAPLANE_MARKER}: {exc} (last {last.get('v')!r})") from exc
Issue-Key: cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-edge-generates-viewer-country-and-forwarded-proto|_warm_edge#1|AssertionError|raise AssertionError(f"{DATAPLANE_MARKER}: {exc} (last {last.get('v')!r})") from exc
Issue-Key: cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-path-prefixes-the-path-the-origin-sees|_warm_edge#1|AssertionError|raise AssertionError(f"{DATAPLANE_MARKER}: {exc} (last {last.get('v')!r})") from exc
Issue-Key: cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-query-string-allexcept-and-all-shape-the-cache-key|_warm_edge#1|AssertionError|raise AssertionError(f"{DATAPLANE_MARKER}: {exc} (last {last.get('v')!r})") from exc
Issue-Key: cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-request-policy-forwards-what-the-cache-key-excludes|_warm_edge#1|AssertionError|raise AssertionError(f"{DATAPLANE_MARKER}: {exc} (last {last.get('v')!r})") from exc
Issue-Key: cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-unlisted-header-cookie-query-blocked-from-origin|_warm_edge#1|AssertionError|raise AssertionError(f"{DATAPLANE_MARKER}: {exc} (last {last.get('v')!r})") from exc
Issue-Key: cloudfront__dynamodb__lambda.deep-cloudfront-3-cf-origin-viewer-spoofed-edge-headers-rejected-at-the-edge|_warm_edge#1|AssertionError|raise AssertionError(f"{DATAPLANE_MARKER}: {exc} (last {last.get('v')!r})") from exc
Issue-Commit: 0970c67cd532b68a1462dc1d98bdb119ad45d0ac

# cloudfront__dynamodb__lambda: the distribution's published DomainName is not an address the emulator serves, so all 8 cf-origin edge checks never reach the edge

> _Note: this issue was identified against the code base at commit `0970c67cd532b68a1462dc1d98bdb119ad45d0ac`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: a Deployed distribution announces a <id>.cloudfront.net DomainName that the emulator does not serve, and no per-distribution listener is published in its place, so no viewer request can ever reach the edge.

AWS: Smithy (cloudfront 2020-05-31) documents Distribution.DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net", and GetDistribution reports Status "Deployed" only once that name serves viewer traffic. In AWS a viewer GET https://<DomainName>/<path> is answered by the edge, which relays to the configured origin (here a Lambda function URL origin) and returns the origin's body plus the edge's cache headers (X-Cache Hit/Miss from cloudfront, Via, Age). Everything these checks assert — Brotli negotiation keyed on Accept-Encoding, a cache Hit leaving the origin invocation counter unchanged, edge-generated CloudFront-Viewer-Country / CloudFront-Forwarded-Proto headers, OriginPath prefixing, query-string AllExcept/All cache keys, origin-request-policy forwarding, unlisted header/cookie/query stripping, and rejection of viewer-spoofed CloudFront-* headers — is observable only through that address.

Emulator: the distribution reaches Deployed and publishes DomainName <id>.cloudfront.net (e.g. d2pysi4346dfp3.cloudfront.net, d5ni6znxjb8do0.cloudfront.net, drkd77tq89jf12.cloudfront.net), a host that is not one of the emulator's own hosts and is not reachable from a customer of the emulator. The warm-up viewer request fails before any assertion with "DATAPLANE_UNIMPLEMENTED: endpoint_url was given https://<id>.cloudfront.net/sim2real-warmup (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". All 8 checks in this service die at that same warm-up step, so not one edge behavior is exercised.

Why: a client that does exactly what AWS documents — create a distribution, wait for Deployed, then fetch https://<DomainName>/<path> — gets no answer at all. The whole CloudFront data plane is unreachable: caching, cache-key shaping, header forwarding and edge-generated headers cannot be used or tested against the emulator even though the control plane reports a healthy, Deployed distribution.

Fix: the CloudFront distribution's published DomainName and its viewer listener — the distribution create/read serializer must announce an address the emulator actually serves (its own host/port, or a per-distribution listener it opens), and that listener must serve viewer requests by relaying to the configured origin and applying the attached cache policy / origin request policy.

Traces: the 8 failing checks listed below, all stopping in _warm_edge with DATAPLANE_UNIMPLEMENTED. Related but separate open issues describe the same shape for other service groups (20261125T052443Z-cloudfront-s3-the-distribution-s-published-domainname-resolves.md, 20261125T091314Z-cloudfront-2-net-new-causes-a-distribution-s.md); neither carries these checks' replay keys.

## Evidence
Expected (AWS): GET https://<DomainName>/sim2real-warmup against a Deployed distribution returns the origin's response through the edge (200 with the Lambda origin's JSON body and X-Cache/Via headers), so the subsequent cache-key, header-forwarding and origin-count assertions can run.

Actual (emulator): the viewer request is never made — the published endpoint is rejected as unserveable: "AssertionError: DATAPLANE_UNIMPLEMENTED: endpoint_url was given https://d2pysi4346dfp3.cloudfront.net/sim2real-warmup (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:46303/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener (last None)". The host label and the loopback port differ per run; the requirement is only that the announced DomainName is an address the emulator serves.

## Steps to Reproduce
1. CreateDistribution with a Lambda function-URL custom origin, a cache policy (e.g. managed CachingDisabled) and an origin request policy; wait until GetDistribution reports Status=Deployed.
2. Read Distribution.DomainName from the create/read response.
3. GET https://<DomainName>/sim2real-warmup.
4. Observe there is no listener for that host: the request cannot be issued/connected, while AWS answers it from the edge.
