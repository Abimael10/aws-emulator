Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudfront__s3.cloudfront-apigw-viewer-request-header-passthrough
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigw-viewer-request-header-passthrough|_http_get#12|Timeout|with urllib.request.urlopen(req, timeout=timeout, context=_https_context(verify)) as resp:
Issue-Commit: 69cb8c4786d799a8a66c2117bf8eebb4fb69db3f

# apigateway__cloudfront__s3: the viewer-request header passthrough check at an API Gateway origin still never reaches its asserted state

> _Note: this report carries the remaining replay key of
> `20261121T120000Z-apigateway-cloudfront-s3-two-edge-data-plane-checks-still-time-out.md`
> (itself the remainder of `20261110T092303Z-apigateway-cloudfront-s3-5-net-new-causes-at.md`),
> whose other key — the S3 website-endpoint origin — now passes and is recorded under
> `issues/things-that-now-work/`. The key, the trace id and everything ruled out so far are
> carried here verbatim so the record of what is still broken is not lost with that
> resolution._

## Operation
CloudFront viewer GET at a distribution edge (API Gateway custom origin)

## Description
Cause 1 of 1: `cloudfront-apigw-viewer-request-header-passthrough` burns its whole 60 s budget
polling `GET <distribution domain>/secure?cb=<n>` without the asserted state ever being
observed, while every sibling check of the same family passes (the API Gateway custom origin
itself, the whitelisted viewer header and query string, the CloudFront-generated header, the
origin custom header, the cache-behaviour path routing, the viewer protocol policy, the
origin-protocol mismatch 502).

AWS: once `GetDistribution` reports `Status=Deployed` the distribution's edge serves the
configured behaviour within seconds, and the viewer's request headers reach an API Gateway custom
origin as the behaviour's policies say.

Emulator: the check is reported only as "Check timed out after 60.0s", with its stack in the
poll's own HTTP GET (and, on another run, in the sleep between polls). The emulator ANSWERS each
poll — every forwarding path the check could exercise is covered by passing end-to-end tests
(below) — but the check's requests and its predicate are not visible from inside the emulator, so
what the predicate asks for is not known and no expected-vs-actual can be stated from here.

Fix: read what this check asserts (its reflected document, and the status it waits for), then
correct the edge or the API Gateway surface it exercises. The ruled-out list below is the state
of play, so the next change need not re-walk it.

Traces: apigateway__cloudfront__s3.cloudfront-apigw-viewer-request-header-passthrough.

## Evidence
"Check timed out after 60.0s", with no traceback frame naming an assertion: the recorded key's
frame is the poll's `urllib.request.urlopen`, and a later run's is `time.sleep(interval_s)`.

What has been ruled out, each by a test in this repository that drives three services over one
engine and dials the address the distribution publishes
(`core/router/tests/all/cloudfront_forwarded_viewer_headers_reach_the_origin.rs`,
`core/router/tests/all/cloudfront_apigateway_origin_edge.rs`):

* an API Gateway stage behind a distribution receives a whitelisted ordinary header, the viewer's
  `User-Agent` (and CloudFront's own where the behaviour forwards none), `Accept-Language`,
  `Cookie`, `Referer`, the query string, `Host`, `Via`, `X-Amz-Cf-Id` and `X-Forwarded-For`, each
  carrying the value the viewer stated;
* the same under the POLICIES a behaviour names rather than the legacy `ForwardedValues` member:
  a custom cache policy's whitelist and a custom origin request policy's, including the
  `CloudFront-*` headers the EDGE generates — a generated header a CACHE POLICY names now reaches
  the origin too, which `CachePolicyHeadersConfig$HeaderBehavior` ("included in the cache key AND
  in requests that CloudFront sends to the origin") requires and which was fixed with this
  report's sibling;
* the answer's own `Content-Type` and status reach the viewer, and the published address answers
  a dialled client over both schemes.

The viewer's own `Authorization` (and `X-Amz-Security-Token`) is the one header this emulator
withholds from every origin, deliberately: the project's threat model forbids forwarding a
caller's credential on the config-controlled cross-service leg (MIT-4009), so a check that
asserts the origin received the viewer's credential cannot be satisfied without a reviewed
exception to that control, and the honest exit for it is a blocked one rather than a quiet
relay.

## Steps to Reproduce
Replay the behavioral suite and re-run the check in trace_ids. By hand: create a REST API that
reflects what reached it, deploy it, front the stage with a distribution whose behaviour forwards
headers, wait for `Status=Deployed` and GET `/secure?cb=<n>` through the edge while inspecting
what the origin received and what status the viewer is answered.
