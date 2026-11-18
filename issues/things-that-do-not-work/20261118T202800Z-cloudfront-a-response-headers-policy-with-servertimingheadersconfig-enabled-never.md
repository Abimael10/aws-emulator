Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.response-headers-policy-server-timing-edge-header
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront.response-headers-policy-server-timing-edge-header|_poll_server_timing#32|Timeout|status, headers = _get(url, verify)
Issue-Commit: 7e4414010feb5d6e6de4259f326eb0449b52de38

# A response headers policy with ServerTimingHeadersConfig enabled never produces a Server-Timing header on the viewer response at the edge

> _Note: this issue was identified against the code base at commit `7e4414010feb5d6e6de4259f326eb0449b52de38`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateResponseHeadersPolicy

## Description
One net-new root cause in the CloudFront edge response path.

=== Cause 1 of 1: the edge ignores the attached response headers policy's ServerTimingHeadersConfig ===
Failing check: cloudfront.response-headers-policy-server-timing-edge-header

AWS: Smithy (com.amazonaws.cloudfront#ResponseHeadersPolicyServerTimingHeadersConfig) models Enabled as a REQUIRED boolean — 'A Boolean that determines whether CloudFront adds the Server-Timing header to HTTP responses that it sends in response to requests that match a cache behavior that's associated with this response headers policy' — and SamplingRate as a double with @range min 0.0 max 100.0, documented as: 'When you set the sampling rate to 100, CloudFront adds the Server-Timing header to the HTTP response for every request that matches the cache behavior that this response headers policy is attached to.' So with Enabled=true and SamplingRate=100.0 on the policy named by DefaultCacheBehavior.ResponseHeadersPolicyId, EVERY 200 viewer response from that behavior carries a Server-Timing header. Its metric names are drawn from the documented set (cdn-cache-hit, cdn-cache-refresh, cdn-cache-miss, cdn-pop, cdn-rid, cdn-hit-layer, cdn-upstream-layer, cdn-upstream-dns, cdn-upstream-connect, cdn-upstream-fbl, cdn-downstream-fbl), and cdn-pop and cdn-rid appear in every documented example whatever the cache outcome. Removing ResponseHeadersPolicyId from the behavior and letting the distribution redeploy makes the header disappear again. Reference: https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/understanding-response-headers-policies.html#server-timing-header

Emulator: the scenario never converges — the check is killed with 'Check timed out after 60.0s' rather than reaching any of its assertions. The check has exactly two unbounded waits after setup: GetDistribution polled until Distribution.Status == 'Deployed', and a 3 s-interval HTTPS GET loop against the distribution's DomainName that only returns once ('Server-Timing' in response headers) == True (every GET is asserted 200 first, and no 200-with-wrong-status assertion fired). Hypothesis: the edge response serializer does not apply the cache behavior's ResponseHeadersPolicy ServerTimingHeadersConfig, so Server-Timing is never added and that GET loop spins until the run budget ends. The only alternative consistent with the same evidence is a distribution that never reports Status 'Deployed'; both stalls live in the same create-distribution/edge-serving path and both must be ruled out to close this.

Why: a client that reads Server-Timing to observe which cache layer served a request, the POP that served it and the edge request id (cdn-pop / cdn-rid) — the documented purpose of this policy feature — gets no such header at all, so cache-efficiency and latency troubleshooting built on it is silently blind. Because the header never appears, a poll-until-present pattern (the documented way to wait out a distribution deployment) hangs instead of failing, so the whole scenario stalls rather than reporting a wrong value.

Fix: the CloudFront data-plane/edge response serializer that applies the matched cache behavior's ResponseHeadersPolicy — specifically the ServerTimingHeadersConfig branch that must add Server-Timing (with documented metric names, always including cdn-pop and cdn-rid) when Enabled=true, honour SamplingRate, and stop adding it once ResponseHeadersPolicyId is removed from the behavior and the distribution redeploys. If instead the stall is GetDistribution never reporting Status 'Deployed' for a distribution created via CreateDistributionWithTags with an OAC S3 origin and the managed CachingDisabled cache policy, the distribution status transition in that handler is the component to fix.

Traces: cloudfront.response-headers-policy-server-timing-edge-header.

## Evidence
cloudfront.response-headers-policy-server-timing-edge-header [mismatch]:

  Error: Check timed out after 60.0s

The check never reached an assertion. Its setup is: create an S3 bucket + index.html, create an OAC (SigningProtocol sigv4, SigningBehavior always, OriginAccessControlOriginType s3), create a response headers policy with ServerTimingHeadersConfig {Enabled: true, SamplingRate: 100.0}, create a distribution via CreateDistributionWithTags with that OAC on the S3 origin, DefaultCacheBehavior using the managed CachingDisabled cache policy (4135ea2d-6df8-44a3-9df3-4b5a84be39ad) and ResponseHeadersPolicyId set to the new policy, then PutBucketPolicy granting cloudfront.amazonaws.com with aws:SourceArn = the distribution ARN.

After setup the only waits are:
  _wait_deployed(cf, dist_id)  -> GetDistribution until Status == 'Deployed'
  _poll_server_timing(url, present=True) -> GET https://<DomainName>/index.html every 3 s, asserting status == 200, until 'Server-Timing' is in the response headers

Expected on AWS: the GET returns 200 with a Server-Timing header whose metric names are a subset of {cdn-cache-hit, cdn-cache-refresh, cdn-cache-miss, cdn-pop, cdn-rid, cdn-hit-layer, cdn-upstream-layer, cdn-upstream-dns, cdn-upstream-connect, cdn-upstream-fbl, cdn-downstream-fbl} and that includes at least cdn-pop and cdn-rid; after removing ResponseHeadersPolicyId from DefaultCacheBehavior and redeploying, the GET returns 200 with NO Server-Timing header.
Actual on the emulator: neither state is ever observed inside the run budget — no Server-Timing-bearing response is produced.

## Steps to Reproduce
1. Create an S3 bucket and put an object 'index.html'.
2. CreateOriginAccessControl {SigningProtocol: sigv4, SigningBehavior: always, OriginAccessControlOriginType: s3}.
3. CreateResponseHeadersPolicy with ResponseHeadersPolicyConfig.ServerTimingHeadersConfig = {Enabled: true, SamplingRate: 100.0}.
4. CreateDistributionWithTags with one S3 origin carrying that OriginAccessControlId, DefaultCacheBehavior {ViewerProtocolPolicy: allow-all, CachePolicyId: 4135ea2d-6df8-44a3-9df3-4b5a84be39ad, ResponseHeadersPolicyId: <policy id>}.
5. PutBucketPolicy allowing s3:GetObject to Service cloudfront.amazonaws.com with Condition StringEquals aws:SourceArn = the distribution ARN.
6. Poll GetDistribution until Status == 'Deployed', then GET https://<DomainName>/index.html.
7. Expected: 200 with a Server-Timing header containing at least cdn-pop and cdn-rid and only documented metric names. Observed: the response carries no Server-Timing header, so a poll-until-present loop never terminates and the scenario is killed by the run budget.
8. Then UpdateDistribution removing ResponseHeadersPolicyId from DefaultCacheBehavior, wait for Deployed, and GET again: AWS returns 200 with no Server-Timing header.
