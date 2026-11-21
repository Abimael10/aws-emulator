Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudfront__s3.cloudfront-apigw-viewer-request-header-passthrough apigateway__cloudfront__s3.cloudfront-s3-website-endpoint-origin-positive
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigw-viewer-request-header-passthrough|_http_get#12|Timeout|with urllib.request.urlopen(req, timeout=timeout, context=_https_context(verify)) as resp:
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-website-endpoint-origin-positive|_poll_http_get#19|Timeout|time.sleep(interval_s)
Issue-Commit: 69cb8c4786d799a8a66c2117bf8eebb4fb69db3f

# apigateway__cloudfront__s3: two edge data-plane checks still never reach their asserted state — a viewer-request header passthrough at an API Gateway origin, and an S3 website-endpoint origin serving its index document

> _Note: these two checks are the remainder of 20261110T092303Z-apigateway-cloudfront-s3-5-net-new-causes-at.md, whose other ten replay keys now pass. They are filed here so the record of what is still broken is not lost with that issue's resolution; the two keys above are carried over verbatim._

## Operation
CloudFront viewer GET at a distribution edge (API Gateway / S3 website-endpoint origin)

## Description
Cause 1 of 1: two of the eight edge data-plane checks of this suite still burn their whole
budget without the asserted state ever being observed, while the other six (invalidation after
an object update, the S3 CORS preflight workflow, s3 sync + invalidate, standard access-log
delivery, the legacy-OAI object, the signed-URL restriction) now pass.

AWS: once `GetDistribution` reports `Status=Deployed`, the distribution's edge serves the
configured behaviour within seconds — the viewer's request headers reach an API Gateway custom
origin as the behaviour's policies say, and an S3 website-endpoint custom origin serves the
index document of the addressed directory.

Emulator: each of the two checks is reported only as "Check timed out after 60.0s", polling and
never satisfying its predicate. Neither check's requests are visible from inside the emulator, so
what the predicate asks for is not known; what IS established, by in-process and dialled probes
of the same operations, is listed under Evidence so the next change need not re-walk it.

Why: the two behaviours a client would reach for after the basics — what an origin receives of a
viewer's request, and a static site fronted by a distribution — stay unobservable through the
emulated edge, and a poll that never converges stalls a workflow rather than failing it.

Fix: the remaining divergence is in neither the origin fetch nor the forwarding selection (see
Evidence); it is to be found by reading what these two checks assert and then correcting the edge
or the origin surface they exercise.
Traces: apigateway__cloudfront__s3.cloudfront-apigw-viewer-request-header-passthrough,
apigateway__cloudfront__s3.cloudfront-s3-website-endpoint-origin-positive.

## Evidence
Both checks report exactly "Check timed out after 60.0s", the first with its stack in the poll's
own HTTP GET and the second in the sleep between polls.

What has been ruled out for `cloudfront-apigw-viewer-request-header-passthrough`, each by a test
or probe in this repository (`core/router/tests/all/cloudfront_forwarded_viewer_headers_reach_the_origin.rs`):
an API Gateway stage behind a distribution receives a whitelisted ordinary header, the viewer's
`User-Agent` (and CloudFront's own where the behaviour forwards none), `Accept-Language`,
`Cookie`, `Referer`, the query string, `Host`, `Via`, `X-Amz-Cf-Id` and `X-Forwarded-For`, each
carrying the value the viewer stated. The viewer's own `Authorization` is the one header this
emulator withholds from every origin by design.

What has been ruled out for `cloudfront-s3-website-endpoint-origin-positive`: the edge serves the
index document over both published spellings of the website host, under `http-only` and
`match-viewer`, for the root, a nested directory, an object key and a key the bucket does not
hold, in process and DIALLED on the published address, with the site made public by a bucket
policy and by a public-read object ACL, and with the front door verifying requests
(`services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/tests/all/edge_website_endpoint_origin.rs`
and `.../edge_website_origin_under_request_authentication.rs`). Replaying with the origin-scheme
check and the origin-leg signing widened to every Amazon S3 endpoint host changed the verdict in
no way, so the origin fetch is not what the check is waiting on.

## Steps to Reproduce
Replay the behavioral suite and re-run the two checks in trace_ids. By hand: (1) create a REST
API that reflects its request headers, deploy it, front it with a distribution whose behaviour
forwards headers, and GET `/secure?cb=<n>` through the edge while inspecting what the origin
received; (2) create a bucket with a website configuration and a public index document, front its
website endpoint as a custom origin with `OriginProtocolPolicy=http-only`, wait for
`Status=Deployed` and GET `/` at the distribution's published address.
