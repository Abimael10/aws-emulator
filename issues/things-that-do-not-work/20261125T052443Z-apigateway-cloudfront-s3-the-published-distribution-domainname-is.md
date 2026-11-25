Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudfront__s3.cfn-deploy-cloudfront-apigw-stack-positive apigateway__cloudfront__s3.cfn-deploy-cloudfront-s3-missing-oac-negative apigateway__cloudfront__s3.cfn-deploy-cloudfront-s3-oac-stack-positive apigateway__cloudfront__s3.cli-apigateway-deploy-stage-cloudfront-origin-workflow apigateway__cloudfront__s3.cli-cloudfront-create-distribution-s3-origin-workflow apigateway__cloudfront__s3.cli-cloudfront-invalidate-cache-after-s3-update apigateway__cloudfront__s3.cli-cloudfront-missing-origin-access-negative apigateway__cloudfront__s3.cli-cloudfront-s3-oac-setup-workflow apigateway__cloudfront__s3.cli-deploy-apigw-cloudfront-distribution-workflow apigateway__cloudfront__s3.cli-s3-cors-cloudfront-preflight-workflow apigateway__cloudfront__s3.cli-s3-sync-cloudfront-invalidate-workflow apigateway__cloudfront__s3.cloudfront-apigateway-cache-behavior-forward-positive apigateway__cloudfront__s3.cloudfront-apigateway-custom-header-passthrough apigateway__cloudfront__s3.cloudfront-apigateway-custom-origin-positive apigateway__cloudfront__s3.cloudfront-apigateway-origin-missing-resource-negative apigateway__cloudfront__s3.cloudfront-apigateway-viewer-protocol-policy-positive apigateway__cloudfront__s3.cloudfront-apigateway-wrong-origin-path-negative apigateway__cloudfront__s3.cloudfront-apigw-cache-behavior-path-routing apigateway__cloudfront__s3.cloudfront-apigw-origin-missing-resource-policy-negative apigateway__cloudfront__s3.cloudfront-apigw-origin-protocol-mismatch-negative apigateway__cloudfront__s3.cloudfront-invalidation-s3-object-update-positive apigateway__cloudfront__s3.cloudfront-s3-oac-origin-access-positive apigateway__cloudfront__s3.cloudfront-s3-oac-origin-positive apigateway__cloudfront__s3.cloudfront-s3-oai-legacy-access-positive apigateway__cloudfront__s3.cloudfront-s3-oai-legacy-positive apigateway__cloudfront__s3.cloudfront-s3-origin-path-prefix-resolution apigateway__cloudfront__s3.cloudfront-s3-signed-url-restricted-access-positive apigateway__cloudfront__s3.cloudfront-s3-website-endpoint-origin-positive apigateway__cloudfront__s3.cloudfront-s3-wrong-oac-signing-negative apigateway__cloudfront__s3.deploy-cloudfront-apigateway-origin-stack-negative apigateway__cloudfront__s3.deploy-cloudfront-s3-oac-stack-positive
Issue-Key: apigateway__cloudfront__s3.cfn-deploy-cloudfront-apigw-stack-positive|_poll_until#9|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cfn-deploy-cloudfront-s3-missing-oac-negative|_poll_until#11|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cfn-deploy-cloudfront-s3-oac-stack-positive|_poll_until#11|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cli-apigateway-deploy-stage-cloudfront-origin-workflow|_http_get#15|URLError|with urllib.request.urlopen(urllib.request.Request(url, headers=headers or {}), timeout=timeout, context=_https_context(verify)) as resp:
Issue-Key: apigateway__cloudfront__s3.cli-cloudfront-create-distribution-s3-origin-workflow|_http_get#19|URLError|with urllib.request.urlopen(urllib.request.Request(url, headers=headers or {}), timeout=timeout, context=_https_context(verify)) as resp:
Issue-Key: apigateway__cloudfront__s3.cli-cloudfront-invalidate-cache-after-s3-update|get#25|ConnectionRefusedError|self.conn.request("GET", self.prefix + path, headers=self.headers)
Issue-Key: apigateway__cloudfront__s3.cli-cloudfront-missing-origin-access-negative|_http_get#17|URLError|with urllib.request.urlopen(urllib.request.Request(url, headers=headers or {}), timeout=timeout, context=_https_context(verify)) as resp:
Issue-Key: apigateway__cloudfront__s3.cli-cloudfront-s3-oac-setup-workflow|_http_get#26|URLError|with urllib.request.urlopen(urllib.request.Request(url, headers=headers or {}), timeout=timeout, context=_https_context(verify)) as resp:
Issue-Key: apigateway__cloudfront__s3.cli-deploy-apigw-cloudfront-distribution-workflow|_poll_http#27|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cli-s3-cors-cloudfront-preflight-workflow|_poll_options#29|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cli-s3-sync-cloudfront-invalidate-workflow|_poll_sticky#30|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigateway-cache-behavior-forward-positive|_poll_http_get#5|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigateway-custom-header-passthrough|_poll_http_get#6|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigateway-custom-origin-positive|_poll_http_get#9|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigateway-origin-missing-resource-negative|_poll_http_get#6|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigateway-viewer-protocol-policy-positive|_poll_http_get#5|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigateway-wrong-origin-path-negative|_poll_http_get#5|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigw-cache-behavior-path-routing|_poll_get#14|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigw-origin-missing-resource-policy-negative|_poll_get#12|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigw-origin-protocol-mismatch-negative|_poll_get#12|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-invalidation-s3-object-update-positive|_poll_http_conn#17|ConnectionRefusedError|conn.connect()
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-oac-origin-access-positive|_poll_http_get#19|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-oac-origin-positive|_poll_http#14|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-oai-legacy-access-positive|_poll_http_get#18|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-oai-legacy-positive|_poll_http#13|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-origin-path-prefix-resolution|_poll_http#14|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-signed-url-restricted-access-positive|_poll_http_get#32|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-website-endpoint-origin-positive|_poll_http_get#19|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-wrong-oac-signing-negative|_poll_http#16|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cloudfront__s3.deploy-cloudfront-apigateway-origin-stack-negative|_get#14|URLError|with urllib.request.urlopen(req, timeout=10, context=_https_context(ctx.http_verify)) as resp:
Issue-Key: apigateway__cloudfront__s3.deploy-cloudfront-s3-oac-stack-positive|_fetch_via_cf#15|URLError|with urllib.request.urlopen(req, timeout=10, context=_https_context(ctx.http_verify)) as resp:
Issue-Commit: 3ce962b3772a5f276bd697ccb3d5b119b45b10c0

# apigateway__cloudfront__s3: the published distribution DomainName is not an address the emulator serves, so 31 edge checks never reach a verdict

> _Note: this issue was identified against the code base at commit `3ce962b3772a5f276bd697ccb3d5b119b45b10c0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: a distribution's published DomainName is not a reachable DNS host, so no request ever reaches the CloudFront edge and every data-plane check in this group either hangs to its 60 s cap or dies on a refused TCP connect.

AWS: Smithy (com-amazonaws-cloudfront-cloudfront2020-05-31) documents Distribution.DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net." It is a bare DNS host name with no path and no port, and once the distribution reaches Deployed a viewer HTTPS GET to https://<DomainName>/<path> is answered by the edge: a cached or origin-fetched 200 with the object's bytes for a granted request, and the edge's own refusal (403 AccessDenied / 404 NoSuchKey, origin-dependent) for a denied one. CreateDistribution is modeled to return that DomainName in its 201 Distribution payload.

Emulator: CreateDistribution answers 201 with a DomainName of the form "127.0.0.1/_aws/cloudfront/<DistributionId>" (observed verbatim in the cloudfront__s3 replay payload: 'DomainName': '127.0.0.1/_aws/cloudfront/<Id>'), while the control plane itself is reached on a different authority (https://127.0.0.1:<control-plane-port>). That value is not a DNS host name at all — it carries a URI path — and the host:port it implies (127.0.0.1:443) has no listener. Every viewer request built from it is refused at connect: "urllib.error.URLError: <urlopen error [Errno 111] Connection refused>", "ConnectionRefusedError: [Errno 111] Connection refused", and via requests "HTTPSConnectionPool(host='127.0.0.1', port=443): Max retries exceeded with url: /_aws/cloudfront/<Id>/ (Caused by NewConnectionError(... [Errno 111] Connection refused))". Checks that poll for a served response instead burn their whole budget and report "Check timed out after 60.0s". So the emulator announces an address other than the one it is reached on, and serves nothing there.

Why: a client that follows AWS's contract — create a distribution, read DomainName, then address the viewer at that host — can never fetch a single byte through the emulated edge. Every behaviour layered on the edge is unverifiable: OAC/OAI origin access (granted and denied), origin-path prefix resolution, S3 website-endpoint origins, signed URLs, invalidation effects, cache-behaviour path routing to an API Gateway origin, custom-header and viewer-protocol passthrough, CORS preflight through the distribution, and the negative cases that must be refused BY the edge rather than by a connect error. Positive and negative cases become indistinguishable, because both fail the same way: nothing answers.

Fix: the CloudFront CreateDistribution/UpdateDistribution handler and its state (the distribution record's published domain) must publish a DomainName that is a host the emulator actually serves — a bare host authority with no path — and the edge data plane for that authority must route a viewer request to the distribution's configured origin and answer it. The same value must be echoed by GetDistribution/ListDistributions readback. File and line citations for the handler belong under Traces.

Note: open issues 20261029T175823Z-apigateway-cloudfront-s3-29-more-cloudfront-api-gateway.md, 20261024T043741Z-apigateway-cloudfront-s3-five-more-cloudfront-api-gateway.md, 20261101T214154Z-apigateway-cloudfront-s3-three-more-edge-eventing-root.md and 20261114T142904Z-cloudfront-reopens-generated-identifiers-and-published-domain-names.md describe neighbouring symptoms, but none of them carries the replay keys of the 31 checks listed below, so those checks are still unowned.

Traces: the 31 failing checks in trace_ids, all in the apigateway__cloudfront__s3 group.

## Evidence
Expected (AWS): CreateDistribution returns DomainName = a DNS host name such as d111111abcdef8.cloudfront.net; an HTTPS GET to https://<DomainName>/<key> after the distribution is Deployed is answered by the edge — 200 with the object's bytes for a granted request, or the edge's own 403 AccessDenied / 404 NoSuchKey for a refused one.

Actual (emulator): DomainName = "127.0.0.1/_aws/cloudfront/<DistributionId>" (a value with a URI path, not a host name), implying 127.0.0.1:443, where nothing listens, while the control plane answers on a different port.
  - requests: ConnectionError(MaxRetryError("HTTPSConnectionPool(host='127.0.0.1', port=443): Max retries exceeded with url: /_aws/cloudfront/<Id>/ (Caused by NewConnectionError('... [Errno 111] Connection refused'))"))
  - urllib: urllib.error.URLError: <urlopen error [Errno 111] Connection refused>
  - socket: ConnectionRefusedError: [Errno 111] Connection refused
  - polling checks: "Error: Check timed out after 60.0s" (no verdict reached)
The host, port and distribution id above are per-run values; the requirement is that the announced DomainName be a host the emulator serves and that the edge answer there.

## Steps to Reproduce
1. Create an S3 bucket and put an object in it (or deploy a REST API stage) to act as the origin.
2. Call cloudfront CreateDistribution with that origin (OAC, OAI or custom origin).
3. Read Distribution.DomainName from the 201 response: it is "127.0.0.1/_aws/cloudfront/<Id>" rather than a DNS host name.
4. Issue an HTTPS GET for the object at https://<DomainName>/<key>.
5. Observe the connect is refused ([Errno 111] Connection refused) instead of an edge response; a poll-until-served loop exhausts its budget and the check times out at 60 s.
6. Re-replay the check ids in trace_ids to confirm.
