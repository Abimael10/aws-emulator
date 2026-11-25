Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__events.xsvc-cloudfront-1-s3-object-created-lambda-invalidation-refreshes-edge
Issue-Key: cloudfront__events.xsvc-cloudfront-1-s3-object-created-lambda-invalidation-refreshes-edge|run#45|PublishedUrlRejected|url, route_headers = ctx.endpoint_route(f"{domain}/page.txt")
Issue-Commit: fbb1a06f1c20de0c01a698c058987963365385aa

# cloudfront__events: the distribution's published DomainName is a <id>.cloudfront.net host the emulator does not serve, so the S3-ObjectCreated → Lambda → CreateInvalidation edge-refresh chain never reaches the edge

> _Note: this issue was identified against the code base at commit `fbb1a06f1c20de0c01a698c058987963365385aa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
One root cause.

Cause 1 of 1: a distribution's announced viewer address is not an address the emulator answers on, so no viewer request can be made through the distribution and the whole S3 Object Created -> EventBridge rule -> Lambda -> CreateInvalidation -> re-fetch chain is ungradable. Failing check: cloudfront__events.xsvc-cloudfront-1-s3-object-created-lambda-invalidation-refreshes-edge.

AWS: Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31) Distribution.DomainName is documented as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net." On AWS that published domain name is a host a viewer can actually connect to: a GET https://<DomainName>/<object key> is served by the edge (origin fetch on a miss, stored copy on a hit, and a fresh origin fetch after CreateInvalidation). The requirement is the general one — whatever address CreateDistribution/GetDistribution publishes as DomainName MUST be an address the emulator itself serves viewer traffic on; the particular host, port and distribution id are per-environment values and are not part of the requirement.

Emulator: the distribution is created and its DomainName is published in the real-AWS form <id>.cloudfront.net (this run: dgzni4pnd7h2s3.cloudfront.net), which the emulator does not listen on. The check's first viewer request against that published host is refused before it leaves the harness: simparity.published.PublishedUrlRejected: "endpoint_url was given https://<id>.cloudfront.net/page.txt (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener" (the 127.0.0.1:<port> value in that sentence is this run's listener, not a requirement). Nothing downstream of the publication is exercised: no cached response, no invalidation effect, no post-invalidation refresh.

Why: a client that does what AWS documents — take Distribution.DomainName and send viewer traffic to it — can never reach the emulated edge at all. Every CDN behaviour built on that address (cache hit/miss, Age, invalidation taking effect, edge functions) is unreachable, and an emulator consumer cannot tell a broken cache from an unreachable edge.

Fix: the CloudFront distribution create/read handlers' DomainName publication (announce an address the emulator serves, as it already does for other published endpoints) together with the edge listener that must answer viewer requests for that address.

Related already-filed issues, named for the fixer and NOT owning this check's replay key (the suite reported this check unowned): open 20261125T053510Z-cloudfront-events-the-s3-object-created-eventbridge-lambda.md records the same check reaching no verdict by exhausting its budget, and the resolved 20261109T181319Z-cloudfront-3-causes-a-distribution-s-domainname-is.md claimed to stop publishing DomainName as <id>.cloudfront.net; this check shows the real-AWS form is published again on this build. They should be closed together with this one.

Traces: cloudfront__events.xsvc-cloudfront-1-s3-object-created-lambda-invalidation-refreshes-edge.

## Evidence
Failing check: cloudfront__events.xsvc-cloudfront-1-s3-object-created-lambda-invalidation-refreshes-edge — verdict mismatch.

Actual (emulator), verbatim from the replay output:
  File "context.py", line 616, in endpoint_route
    return self._published_target(url, "endpoint_url"), {}
  File "context.py", line 632, in _published_target
    raise PublishedUrlRejected(...)
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net/page.txt (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:40707/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener

Expected (AWS): CreateDistribution/GetDistribution publish a DomainName that serves viewer traffic — GET https://<DomainName>/page.txt returns the S3 origin object's bytes (200), a repeat is served from the edge, and after the rule-invoked Lambda's CreateInvalidation the next GET returns the updated bytes.

Per-run values that are NOT part of the requirement: the distribution id in the published host (dgzni4pnd7h2s3), the emulator's listener 127.0.0.1:40707, the object key, bucket, function and rule names. The requirement is that the published DomainName is an address the emulator serves.

## Steps to Reproduce
1. CreateDistribution with an S3 origin holding an object (e.g. page.txt) and a cache behavior; wait until the distribution reports Deployed.
2. Read Distribution.DomainName from the create/get response.
3. Send a viewer GET for the object to https://<DomainName>/<key>.
   Expected: 200 with the origin object's bytes, served by the emulator's edge.
   Observed: the published host is <id>.cloudfront.net, which is not an address the emulator serves, so the request is rejected before any viewer traffic is attempted (PublishedUrlRejected) and the remaining S3-event -> EventBridge -> Lambda -> CreateInvalidation -> re-fetch steps are never exercised.
