Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.response-headers-policy-server-timing-edge-header
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront.response-headers-policy-server-timing-edge-header|run#36|AssertionError|assert _ALWAYS_PRESENT <= names, (
Issue-Commit: 93aea946e1be9d371a5c94c0146ea55c9b1afa4f

# Edge Server-Timing header omits the cdn-pop metric when ServerTimingHeadersConfig is enabled

> _Note: this issue was identified against the code base at commit `93aea946e1be9d371a5c94c0146ea55c9b1afa4f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateResponseHeadersPolicy

## Description
One net-new root cause in the CloudFront edge viewer-response path. Related but NOT covered by the open issue 20261118T202800Z-cloudfront-a-response-headers-policy-with-servertimingheadersconfig-enabled-never.md: that issue describes the edge emitting NO Server-Timing header at all (the check stalled in its poll-until-present loop and was killed by the run budget). The emulator now DOES emit a Server-Timing header and the check reaches its assertions, so this is a different, later failure point in the same feature — the header's metric set is incomplete.

=== Cause 1 of 1: the edge's Server-Timing header never carries the cdn-pop metric ===
Failing check: cloudfront.response-headers-policy-server-timing-edge-header

AWS: Smithy (com.amazonaws.cloudfront#ResponseHeadersPolicyServerTimingHeadersConfig) documents this config as '<p>A configuration for enabling the <code>Server-Timing</code> header in HTTP responses sent from CloudFront. CloudFront adds this header to HTTP responses that it sends in response to requests that match a cache behavior that's associated with this response headers policy.</p>' and points at the Server-Timing header reference (https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/understanding-response-headers-policies.html#server-timing-header). Enabled is a required boolean and SamplingRate is a double @range 0.0-100.0, where 100 means every matching request gets the header. The documented metric vocabulary is cdn-cache-hit, cdn-cache-refresh, cdn-cache-miss, cdn-pop, cdn-rid, cdn-hit-layer, cdn-upstream-layer, cdn-upstream-dns, cdn-upstream-connect, cdn-upstream-fbl, cdn-downstream-fbl. Two of those are documentation-wide invariants rather than cache-outcome-dependent: cdn-pop (the name of the CloudFront point of presence that served the request, carried as `cdn-pop;desc="<POP name>"`) and cdn-rid (the edge request id, `cdn-rid;desc="<request id>"`); every documented example of the header carries both whatever the cache outcome was. So with Enabled=true and SamplingRate=100.0 on the policy named by DefaultCacheBehavior.ResponseHeadersPolicyId, EVERY 200 viewer response from that behavior carries a Server-Timing header that includes a cdn-pop metric alongside cdn-rid.

Emulator: the viewer response does carry Server-Timing, and all the names it emits are inside the documented vocabulary, but cdn-pop is never among them. The replayed response header was of the form `cdn-rid;desc="<32 hex chars>", cdn-cache-miss, cdn-upstream-layer;desc="EDGE", cdn-upstream-fbl;dur=<ms>, cdn-downstream-fbl;dur=<ms>` — cdn-rid, the cache-outcome metric, the upstream-layer metric and the two first-byte-latency metrics are present; the POP identity metric is absent. (The request id string and the dur values are per-run values, not requirements; the missing NAME is the defect.) The check fails with AssertionError: Server-Timing is missing ['cdn-pop'].

Why: cdn-pop is the only way a client learns WHICH point of presence served a request. A client that reads Server-Timing to attribute latency or cache behavior to an edge location — the documented purpose of this feature — sees a well-formed header and therefore has no signal that the field is missing; it silently attributes every request to an unknown POP, and any dashboard or assertion keyed on cdn-pop breaks against the emulator while passing against AWS.

Fix: the CloudFront edge response serializer branch that builds the Server-Timing header value when the matched cache behavior's ResponseHeadersPolicy has ServerTimingHeadersConfig.Enabled=true — it must append a cdn-pop metric (with the serving POP's name in its desc parameter, from the emulator's edge/POP state) in every sampled response, not only cdn-rid and the latency/cache metrics.

Traces: cloudfront.response-headers-policy-server-timing-edge-header.

## Evidence
cloudfront.response-headers-policy-server-timing-edge-header [mismatch]:

  File "check_cloudfront_response_headers_policy_server_timing_edge_header.py", line 221, in run
    assert _ALWAYS_PRESENT <= names, (
  AssertionError: Server-Timing is missing ['cdn-pop'], present in every documented example: 'cdn-rid;desc="c584133ac916ab3c3ee5789041c98ac3", cdn-cache-miss, cdn-upstream-layer;desc="EDGE", cdn-upstream-fbl;dur=4, cdn-downstream-fbl;dur=4'

Expected (AWS): GET https://<DomainName>/<key> returns 200 with a Server-Timing header whose metric names are a subset of {cdn-cache-hit, cdn-cache-refresh, cdn-cache-miss, cdn-pop, cdn-rid, cdn-hit-layer, cdn-upstream-layer, cdn-upstream-dns, cdn-upstream-connect, cdn-upstream-fbl, cdn-downstream-fbl} AND includes at least cdn-pop and cdn-rid, e.g. `cdn-pop;desc="<POP name>", cdn-rid;desc="<edge request id>", cdn-cache-miss, cdn-upstream-layer;desc="EDGE", cdn-upstream-fbl;dur=<ms>, cdn-downstream-fbl;dur=<ms>`. Fixed text here is the metric names and their parameter keys (desc=, dur=); the POP name, request id and durations are values from the serving edge and the request.
Actual (emulator): 200 with a Server-Timing header carrying cdn-rid, cdn-cache-miss, cdn-upstream-layer, cdn-upstream-fbl and cdn-downstream-fbl, and no cdn-pop metric at all. The subset check against the documented vocabulary passes; only the always-present check fails.

The second half of the check (removing ResponseHeadersPolicyId from DefaultCacheBehavior and asserting the header disappears) is never reached because of this assertion.

## Steps to Reproduce
1. Create an S3 bucket and put an object 'index.html'.
2. CreateOriginAccessControl {SigningProtocol: sigv4, SigningBehavior: always, OriginAccessControlOriginType: s3}.
3. CreateResponseHeadersPolicy with ResponseHeadersPolicyConfig.ServerTimingHeadersConfig = {Enabled: true, SamplingRate: 100.0}.
4. CreateDistributionWithTags with one S3 origin carrying that OriginAccessControlId and DefaultCacheBehavior {ViewerProtocolPolicy: allow-all, CachePolicyId: 4135ea2d-6df8-44a3-9df3-4b5a84be39ad (managed CachingDisabled), ResponseHeadersPolicyId: <policy id>}.
5. PutBucketPolicy allowing s3:GetObject to Service cloudfront.amazonaws.com with Condition StringEquals aws:SourceArn = the distribution ARN.
6. Poll GetDistribution until Distribution.Status == 'Deployed'.
7. GET https://<DomainName>/index.html and read the Server-Timing response header.
8. Expected: the header includes a cdn-pop metric (and a cdn-rid metric), with every name drawn from the documented metric set. Observed: the header includes cdn-rid and the cache/upstream/latency metrics but no cdn-pop.
