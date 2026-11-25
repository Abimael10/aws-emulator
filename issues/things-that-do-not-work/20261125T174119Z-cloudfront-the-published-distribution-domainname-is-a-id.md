Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.cache-behavior-first-listed-match-wins-over-more-specific-pattern cloudfront.cf-cache-key-querystring-allowlist-separates-cache-entries cloudfront.cf-compression-serves-gzip-only-when-accept-encoding-present cloudfront.cf-continuous-deployment-single-header-routes-to-staging-distribution cloudfront.cf-fetch-through-distribution cloudfront.cf-response-headers-policy-adds-and-overrides-headers-at-edge cloudfront.cf-unsigned-request-to-trusted-key-group-distribution-rejected cloudfront.cf-wildcard-invalidation-clears-only-matching-objects cloudfront.cfcache-conditional-get-304-only-for-the-matching-etag cloudfront.cfcache-cookie-allowlist-separates-cache-entries cloudfront.cfcache-min-ttl-floors-origin-no-cache-directive cloudfront.cfcache-range-beyond-object-rejected-and-partial-served-from-cache cloudfront.continuous-deployment-policy-disabled-suppresses-header-routing cloudfront.custom-error-response-rewrites-status-and-caches-for-error-caching-min-ttl cloudfront.deep-cloudfront-1-cf-edge-fn-lambda-edge-unqualified-arn-rejected cloudfront.deep-cloudfront-1-cf-edge-fn-throw-yields-503-rejected cloudfront.deep-cloudfront-1-cf-edge-fn-viewer-request-rewrite cloudfront.deep-cloudfront-1-cf-edge-fn-viewer-response-header cloudfront.deploy-cache-policy-cfn-enforced-on-distribution cloudfront.deploy-cache-policy-cfn-ttl-enforced cloudfront.deploy-cache-policy-enforced-via-cloudformation cloudfront.deploy-cloudfront-distribution-serves-origin-content cloudfront.deploy-distribution-cfn-serves-live-content cloudfront.deploy-distribution-cfn-serves-live-traffic cloudfront.deploy-distribution-serves-content-via-cloudformation cloudfront.e2e-distribution-invalidation-cache-refresh cloudfront.e2e-distribution-webacl-association-blocks-request cloudfront.e2e-invalidation-clears-cached-content cloudfront.e2e-invalidation-clears-cached-object cloudfront.e2e-invalidation-refreshes-stale-cached-object cloudfront.e2e-webacl-association-blocks-then-unblocks-requests cloudfront.edge-requests-deliver-requests-metric-with-global-dimension cloudfront.https-only-and-disallowed-method-rejected-at-edge cloudfront.max-ttl-clamps-origin-cache-control-max-age cloudfront.origin-group-fails-over-to-secondary-origin-at-the-edge cloudfront.origin-request-policy-header-does-not-enter-the-cache-key cloudfront.response-headers-policy-server-timing-edge-header cloudfront.server-side-origin-access-control-signs-requests cloudfront.server-side-security-headers-policy-enforced-on-response cloudfront.signed-cookies-custom-policy-admits-wildcard-and-expired-policy-refused cloudfront.signed-url-key-outside-trusted-key-group-rejected cloudfront.trusted-key-group-on-ordered-behavior-gates-only-its-path-pattern cloudfront.web-acl-association-enforced-at-edge
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront.cache-behavior-first-listed-match-wins-over-more-specific-pattern|run#28|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.cf-cache-key-querystring-allowlist-separates-cache-entries|run#23|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.cf-compression-serves-gzip-only-when-accept-encoding-present|run#20|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.cf-continuous-deployment-single-header-routes-to-staging-distribution|run#28|PublishedUrlRejected|edge, route = ctx.endpoint_route(primary['DomainName'])
Issue-Key: cloudfront.cf-fetch-through-distribution|run#24|PublishedUrlRejected|url, route_headers = ctx.endpoint_route(f"{domain}/{key}")
Issue-Key: cloudfront.cf-response-headers-policy-adds-and-overrides-headers-at-edge|run#23|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.cf-unsigned-request-to-trusted-key-group-distribution-rejected|run#24|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.cf-wildcard-invalidation-clears-only-matching-objects|run#18|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.cfcache-conditional-get-304-only-for-the-matching-etag|run#22|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.cfcache-cookie-allowlist-separates-cache-entries|run#23|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.cfcache-min-ttl-floors-origin-no-cache-directive|run#28|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.cfcache-range-beyond-object-rejected-and-partial-served-from-cache|run#21|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.continuous-deployment-policy-disabled-suppresses-header-routing|run#29|PublishedUrlRejected|edge, route_headers = ctx.endpoint_route(primary["DomainName"])
Issue-Key: cloudfront.custom-error-response-rewrites-status-and-caches-for-error-caching-min-ttl|run#25|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.deep-cloudfront-1-cf-edge-fn-lambda-edge-unqualified-arn-rejected|run#30|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.deep-cloudfront-1-cf-edge-fn-throw-yields-503-rejected|run#10|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.deep-cloudfront-1-cf-edge-fn-viewer-request-rewrite|run#11|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.deep-cloudfront-1-cf-edge-fn-viewer-response-header|run#10|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.deploy-cache-policy-cfn-enforced-on-distribution|run#14|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.deploy-cache-policy-cfn-ttl-enforced|run#28|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.deploy-cache-policy-enforced-via-cloudformation|run#19|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain_name)
Issue-Key: cloudfront.deploy-cloudfront-distribution-serves-origin-content|run#10|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain_name)
Issue-Key: cloudfront.deploy-distribution-cfn-serves-live-content|_poll_fetch#22|PublishedUrlRejected|_url, _route = ctx.endpoint_route(url)
Issue-Key: cloudfront.deploy-distribution-cfn-serves-live-traffic|run#8|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.deploy-distribution-serves-content-via-cloudformation|run#15|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain_name)
Issue-Key: cloudfront.e2e-distribution-invalidation-cache-refresh|_fetch#26|PublishedUrlRejected|_url, _route = ctx.endpoint_route(url)
Issue-Key: cloudfront.e2e-distribution-webacl-association-blocks-request|run#21|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain_name)
Issue-Key: cloudfront.e2e-invalidation-clears-cached-content|run#8|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain_name)
Issue-Key: cloudfront.e2e-invalidation-clears-cached-object|run#13|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain_name)
Issue-Key: cloudfront.e2e-invalidation-refreshes-stale-cached-object|run#17|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.e2e-webacl-association-blocks-then-unblocks-requests|run#18|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.edge-requests-deliver-requests-metric-with-global-dimension|run#25|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.https-only-and-disallowed-method-rejected-at-edge|run#22|PublishedUrlRejected|plain, plain_route = ctx.endpoint_route(f"http://{domain}")
Issue-Key: cloudfront.max-ttl-clamps-origin-cache-control-max-age|run#23|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.origin-group-fails-over-to-secondary-origin-at-the-edge|run#28|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.origin-request-policy-header-does-not-enter-the-cache-key|run#25|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.response-headers-policy-server-timing-edge-header|run#31|PublishedUrlRejected|url = f"{ctx.endpoint_url(domain_name)}/{key}"
Issue-Key: cloudfront.server-side-origin-access-control-signs-requests|run#21|PublishedUrlRejected|edge, route = ctx.endpoint_route(dist_domain)
Issue-Key: cloudfront.server-side-security-headers-policy-enforced-on-response|run#11|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.signed-cookies-custom-policy-admits-wildcard-and-expired-policy-refused|run#28|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.signed-url-key-outside-trusted-key-group-rejected|run#26|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain)
Issue-Key: cloudfront.trusted-key-group-on-ordered-behavior-gates-only-its-path-pattern|run#30|PublishedUrlRejected|base, extra = ctx.endpoint_route(edge_host)
Issue-Key: cloudfront.web-acl-association-enforced-at-edge|run#4|PublishedUrlRejected|edge, route = ctx.endpoint_route(domain_name)
Issue-Commit: a11be7ebac31f84405a5fd36d8a18986542b4650

# cloudfront: the published Distribution DomainName is a <id>.cloudfront.net host the emulator does not serve, so 43 edge data-plane checks are refused before any viewer request is made

> _Note: this issue was identified against the code base at commit `a11be7ebac31f84405a5fd36d8a18986542b4650`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
REOPENS 20261113T163353Z-cloudfront-a-cloudformation-deployed-distribution-s-domainname-attribute.md: its fix left the call refused. That issue reported the published DomainName as an <id>.cloudfront.net host the emulator does not serve; the member is again exactly that, so this is the same defect in a new form, not a new one. The related open issue 20261125T091314Z-cloudfront-2-net-new-causes-a-distribution-s.md describes the previous shape of the value ('<authority>/_aws/cloudfront/<Id>', which failed the host-name test and produced connect refusals/timeouts); the suite does not mark these checks as owned by it because the published value, and therefore the failure, changed: DomainName is now a well-formed DNS host name, but one that belongs to real AWS and is not an address this emulator answers on. Filing it here and naming that issue as instructed.

Cause 1 of 1: CreateDistribution/GetDistribution publish Distribution.DomainName as a real-AWS <id>.cloudfront.net host name, which is not one of the emulator's own hosts, so no viewer request can ever be addressed to the distribution's edge.

AWS: Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31) documents Distribution$DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net" — a bare DNS host name, no scheme and no path. In AWS that host name is the address the edge actually answers viewer requests on: once GetDistribution reports Status 'Deployed', an HTTPS GET for an object that exists at the configured origin, addressed to that host, is answered 200 with the origin's bytes and the configured edge behavior applied on top (cache-policy TTL clamping and MinTTL floors, query-string/cookie cache-key allowlists, compression, conditional GET and Range, CustomErrorResponses, response-headers policies, CloudFront Functions at viewer request/response, signed URLs and cookies and trusted key groups, continuous-deployment header routing, invalidation, method/geo restrictions, web-ACL enforcement, request metrics and access logs). Recorded behavior only for the viewer-side specifics: the edge is not modeled in Smithy.

Emulator: the member is a well-formed host name but it is an address the emulator does not serve. The replay harness refuses to route it rather than silently reaching real AWS: 'endpoint_url was given https://<id>.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener' (simparity.published.PublishedUrlRejected). The fixed text of that message is the requirement being violated; the host label, the port and the object path are per-run values. All 43 checks below fail at the first viewer request, so nothing about the edge is observable.

Why: a client that creates a distribution and then fetches through it has no reachable address. The one value AWS gives it for that purpose points away from the emulator, so every CloudFront capability that is only visible from the viewer side — content delivery, cache semantics, signed access, error pages, invalidation, metrics and logs — is unreachable, and a client that composes 'https://' + DomainName + '/' + key leaves the emulator entirely.

Fix: the CloudFront distribution state/serializer that mints the published domain name, together with the edge listener it must name. The DomainName a distribution publishes must be a host the emulator answers viewer requests on (one of its own hosts, or a per-resource listener it stands up for that distribution), and the viewer pipeline on that listener must resolve the matching cache behavior, fetch from the configured origin and apply cache/policy/signed-access/error handling.

Traces: the 43 failing check ids in trace_ids; representative ones are cloudfront.cf-fetch-through-distribution, cloudfront.deploy-distribution-cfn-serves-live-content and cloudfront.web-acl-association-enforced-at-edge.

## Evidence
Expected: Distribution.DomainName is a DNS host name the emulator answers on, so an HTTPS GET https://<DomainName>/<key> against a Deployed distribution returns 200 with the origin object's bytes and the configured edge behavior applied.

Actual (all 43 checks, identical exception type and message shape):
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:46717/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
    (cloudfront.deploy-cloudfront-distribution-serves-origin-content)
  simparity.published.PublishedUrlRejected: endpoint_url was given http://d5ni6znxjb8do0.cloudfront.net/page.txt ... needs http(s)://127.0.0.1:33471/<resource path> ...
    (cloudfront.e2e-distribution-invalidation-cache-refresh)
  simparity.published.PublishedUrlRejected: endpoint_url was given https://d2pysi4346dfp3.cloudfront.net ... needs http(s)://127.0.0.1:36067/<resource path> ...
    (cloudfront.cache-behavior-first-listed-match-wins-over-more-specific-pattern)

The host labels (dgzni4pnd7h2s3, d5ni6znxjb8do0, d2pysi4346dfp3, ...), the loopback port and the object paths are per-run values. The invariant divergence is that the published host is in the cloudfront.net namespace of real AWS instead of an address this emulator is reached on.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (run_behavioral_suite).
2. Replay cloudfront.cf-fetch-through-distribution: CreateDistribution over a reachable origin, poll GetDistribution until Status == 'Deployed', then read Distribution.DomainName.
3. Observe the value is <id>.cloudfront.net — a host the emulator does not listen on. Any viewer GET addressed to it is refused by the harness (PublishedUrlRejected) instead of reaching the emulator's edge.
4. Repeat with any other id in trace_ids; every one fails at its first viewer request.
