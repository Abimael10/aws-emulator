Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudfront__s3.cfn-deploy-cloudfront-apigw-stack-positive apigateway__cloudfront__s3.cfn-deploy-cloudfront-s3-missing-oac-negative apigateway__cloudfront__s3.cfn-deploy-cloudfront-s3-oac-stack-positive apigateway__cloudfront__s3.cfn-stack-update-cloudfront-origin-positive apigateway__cloudfront__s3.cli-apigateway-deploy-stage-cloudfront-origin-workflow apigateway__cloudfront__s3.cli-cloudfront-create-distribution-s3-origin-workflow apigateway__cloudfront__s3.cli-cloudfront-invalidate-cache-after-s3-update apigateway__cloudfront__s3.cli-cloudfront-missing-origin-access-negative apigateway__cloudfront__s3.cli-cloudfront-s3-oac-setup-workflow apigateway__cloudfront__s3.cli-deploy-apigw-cloudfront-distribution-workflow apigateway__cloudfront__s3.cli-s3-cors-cloudfront-preflight-workflow apigateway__cloudfront__s3.cli-s3-sync-cloudfront-invalidate-workflow apigateway__cloudfront__s3.cloudfront-apigateway-cache-behavior-forward-positive apigateway__cloudfront__s3.cloudfront-apigateway-custom-header-passthrough apigateway__cloudfront__s3.cloudfront-apigateway-custom-origin-positive apigateway__cloudfront__s3.cloudfront-apigateway-origin-missing-resource-negative apigateway__cloudfront__s3.cloudfront-apigateway-viewer-protocol-policy-positive apigateway__cloudfront__s3.cloudfront-apigateway-wrong-origin-path-negative apigateway__cloudfront__s3.cloudfront-apigw-cache-behavior-path-routing apigateway__cloudfront__s3.cloudfront-apigw-custom-header-forward-positive apigateway__cloudfront__s3.cloudfront-apigw-origin-missing-resource-policy-negative apigateway__cloudfront__s3.cloudfront-apigw-origin-positive apigateway__cloudfront__s3.cloudfront-apigw-origin-protocol-mismatch-negative apigateway__cloudfront__s3.cloudfront-apigw-viewer-request-header-passthrough apigateway__cloudfront__s3.cloudfront-invalidation-s3-object-update-positive apigateway__cloudfront__s3.cloudfront-s3-access-logging-delivery-positive apigateway__cloudfront__s3.cloudfront-s3-bucket-policy-wrong-distribution-negative apigateway__cloudfront__s3.cloudfront-s3-missing-oac-policy-negative apigateway__cloudfront__s3.cloudfront-s3-oac-origin-access-positive apigateway__cloudfront__s3.cloudfront-s3-oac-origin-positive apigateway__cloudfront__s3.cloudfront-s3-oai-legacy-access-positive apigateway__cloudfront__s3.cloudfront-s3-oai-legacy-positive apigateway__cloudfront__s3.cloudfront-s3-origin-mismatched-bucket-region-negative apigateway__cloudfront__s3.cloudfront-s3-origin-missing-bucket-policy-negative apigateway__cloudfront__s3.cloudfront-s3-origin-path-prefix-resolution apigateway__cloudfront__s3.cloudfront-s3-public-bucket-direct-access-bypass apigateway__cloudfront__s3.cloudfront-s3-public-bucket-direct-access-negative apigateway__cloudfront__s3.cloudfront-s3-signed-url-restricted-access-positive apigateway__cloudfront__s3.cloudfront-s3-website-endpoint-origin-positive apigateway__cloudfront__s3.cloudfront-s3-wrong-oac-signing-negative apigateway__cloudfront__s3.deploy-cloudfront-apigateway-origin-stack-negative apigateway__cloudfront__s3.deploy-cloudfront-s3-oac-stack-positive apigateway__cloudfront__s3.origin-group-apigateway-mock-primary-fails-over-to-s3-secondary
Issue-Key: apigateway__cloudfront__s3.cfn-deploy-cloudfront-apigw-stack-positive|_fetch_via_cloudfront#9|PublishedUrlRejected|url, headers = ctx.endpoint_route(f"{domain}/")
Issue-Key: apigateway__cloudfront__s3.cfn-deploy-cloudfront-s3-missing-oac-negative|_fetch#11|PublishedUrlRejected|url, headers = ctx.endpoint_route(f"{domain}/index.html")
Issue-Key: apigateway__cloudfront__s3.cfn-deploy-cloudfront-s3-oac-stack-positive|_fetch_via_cloudfront#11|PublishedUrlRejected|url, headers = ctx.endpoint_route(f"{domain}/index.html")
Issue-Key: apigateway__cloudfront__s3.cfn-stack-update-cloudfront-origin-positive|_fetch#13|PublishedUrlRejected|url, headers = ctx.endpoint_route(f"{domain}/index.html")
Issue-Key: apigateway__cloudfront__s3.cli-apigateway-deploy-stage-cloudfront-origin-workflow|run#14|PublishedUrlRejected|cf_url, cf_headers = ctx.endpoint_route(f"{dist['domain']}/hello")
Issue-Key: apigateway__cloudfront__s3.cli-cloudfront-create-distribution-s3-origin-workflow|run#18|PublishedUrlRejected|url, headers = ctx.endpoint_route(f"{dist['domain']}/{key}")
Issue-Key: apigateway__cloudfront__s3.cli-cloudfront-invalidate-cache-after-s3-update|run#22|PublishedUrlRejected|edge_url, edge_headers = ctx.endpoint_route(dist["domain"])
Issue-Key: apigateway__cloudfront__s3.cli-cloudfront-missing-origin-access-negative|run#16|PublishedUrlRejected|url, headers = ctx.endpoint_route(f"{dist['domain']}/{key}")
Issue-Key: apigateway__cloudfront__s3.cli-cloudfront-s3-oac-setup-workflow|run#25|PublishedUrlRejected|cf_url, cf_headers = ctx.endpoint_route(f"{dist['domain']}/{key}")
Issue-Key: apigateway__cloudfront__s3.cli-deploy-apigw-cloudfront-distribution-workflow|run#26|PublishedUrlRejected|edge_url, edge_headers = ctx.endpoint_route(f"{dist_domain}/")
Issue-Key: apigateway__cloudfront__s3.cli-s3-cors-cloudfront-preflight-workflow|run#28|PublishedUrlRejected|edge_url, edge_headers = ctx.endpoint_route(f"{dist_domain}/asset.txt")
Issue-Key: apigateway__cloudfront__s3.cli-s3-sync-cloudfront-invalidate-workflow|run#27|PublishedUrlRejected|edge_url, edge_headers = ctx.endpoint_route(dist_domain)
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigateway-cache-behavior-forward-positive|_http_get#5|PublishedUrlRejected|target, route = ctx.endpoint_route(host if scheme is None else f"{scheme}://{host}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigateway-custom-header-passthrough|_http_get#6|PublishedUrlRejected|target, route = ctx.endpoint_route(host if scheme is None else f"{scheme}://{host}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigateway-custom-origin-positive|_http_get#9|PublishedUrlRejected|target, route = ctx.endpoint_route(host if scheme is None else f"{scheme}://{host}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigateway-origin-missing-resource-negative|_http_get#6|PublishedUrlRejected|target, route = ctx.endpoint_route(host if scheme is None else f"{scheme}://{host}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigateway-viewer-protocol-policy-positive|_http_get#5|PublishedUrlRejected|target, route = ctx.endpoint_route(host if scheme is None else f"{scheme}://{host}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigateway-wrong-origin-path-negative|_http_get#5|PublishedUrlRejected|target, route = ctx.endpoint_route(host if scheme is None else f"{scheme}://{host}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigw-cache-behavior-path-routing|run#12|PublishedUrlRejected|edge, edge_headers = ctx.endpoint_route(dist["domain"])
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigw-custom-header-forward-positive|run#11|PublishedUrlRejected|url, edge_headers = ctx.endpoint_route(f"{dist['domain']}/probe?cb={token}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigw-origin-missing-resource-policy-negative|run#10|PublishedUrlRejected|edge, edge_headers = ctx.endpoint_route(dist["domain"])
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigw-origin-positive|run#10|PublishedUrlRejected|url, edge_headers = ctx.endpoint_route(f"{dist['domain']}/hello?cb={token}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigw-origin-protocol-mismatch-negative|run#10|PublishedUrlRejected|edge, edge_headers = ctx.endpoint_route(dist["domain"])
Issue-Key: apigateway__cloudfront__s3.cloudfront-apigw-viewer-request-header-passthrough|run#11|PublishedUrlRejected|url, edge_headers = ctx.endpoint_route(f"{dist['domain']}/secure?cb={cb}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-invalidation-s3-object-update-positive|run#12|PublishedUrlRejected|url, route_headers = ctx.endpoint_route(f"{domain_name}/{key}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-access-logging-delivery-positive|run#22|PublishedUrlRejected|request_url, edge_headers = ctx.endpoint_route(f"{domain}/{key}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-bucket-policy-wrong-distribution-negative|run#20|PublishedUrlRejected|url, edge_headers = ctx.endpoint_route(f"{domain}/{key}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-missing-oac-policy-negative|run#18|PublishedUrlRejected|url, edge_headers = ctx.endpoint_route(f"{domain}/{key}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-oac-origin-access-positive|run#18|PublishedUrlRejected|edge_url, edge_headers = ctx.endpoint_route(f"{domain}/{key}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-oac-origin-positive|run#13|PublishedUrlRejected|edge_url, edge_headers = ctx.endpoint_route(f"{domain_name}/{key}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-oai-legacy-access-positive|run#17|PublishedUrlRejected|edge_url, edge_headers = ctx.endpoint_route(f"{domain}/{key}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-oai-legacy-positive|run#12|PublishedUrlRejected|edge_url, edge_headers = ctx.endpoint_route(f"{domain_name}/{key}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-origin-mismatched-bucket-region-negative|run#13|PublishedUrlRejected|url, route_headers = ctx.endpoint_route(f"{domain_name}/{key}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-origin-missing-bucket-policy-negative|run#10|PublishedUrlRejected|url, route_headers = ctx.endpoint_route(f"{domain_name}/{key}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-origin-path-prefix-resolution|run#13|PublishedUrlRejected|edge_url, edge_headers = ctx.endpoint_route(f"{domain_name}/index.html")
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-public-bucket-direct-access-bypass|run#12|PublishedUrlRejected|cf_url, cf_headers = ctx.endpoint_route(f"{domain_name}/{key}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-public-bucket-direct-access-negative|run#24|PublishedUrlRejected|cf_url, cf_headers = ctx.endpoint_route(f"{domain}/{key}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-signed-url-restricted-access-positive|run#26|PublishedUrlRejected|unsigned_url, edge_headers = ctx.endpoint_route(f"{domain}/{key}")
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-website-endpoint-origin-positive|run#18|PublishedUrlRejected|edge_url, edge_headers = ctx.endpoint_route(f"{domain}/")
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-wrong-oac-signing-negative|run#14|PublishedUrlRejected|base, route = ctx.endpoint_route(domain_name)
Issue-Key: apigateway__cloudfront__s3.deploy-cloudfront-apigateway-origin-stack-negative|_get#14|PublishedUrlRejected|base, route = ctx.endpoint_route(domain)
Issue-Key: apigateway__cloudfront__s3.deploy-cloudfront-s3-oac-stack-positive|_fetch_via_cf#15|PublishedUrlRejected|base, route = ctx.endpoint_route(domain)
Issue-Key: apigateway__cloudfront__s3.origin-group-apigateway-mock-primary-fails-over-to-s3-secondary|run#23|PublishedUrlRejected|edge, route = ctx.endpoint_route(dist["DomainName"])
Issue-Commit: a11be7ebac31f84405a5fd36d8a18986542b4650

# apigateway__cloudfront__s3: the distribution DomainName published for S3 and API Gateway origins is an unserved <id>.cloudfront.net host, so 43 edge checks are refused before the first viewer request

> _Note: this issue was identified against the code base at commit `a11be7ebac31f84405a5fd36d8a18986542b4650`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
The open issue 20261125T052443Z-apigateway-cloudfront-s3-the-published-distribution-domainname-is.md describes the same family of failures for an earlier form of the published value and does not carry the replay keys of the 43 checks below (the suite marks none of them owned), so this is filed separately and names that issue here as instructed.

Cause 1 of 1: every distribution the emulator creates in this suite — over an S3 origin (OAC, OAI or website endpoint), over an API Gateway REST API custom origin, and via CloudFormation or the CLI — publishes Distribution.DomainName as a real-AWS <id>.cloudfront.net host name that the emulator does not answer on, so no viewer request to the edge can be addressed at all.

AWS: Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31) documents Distribution$DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net"; the CloudFormation AWS::CloudFront::Distribution DomainName attribute and the CLI's create-distribution output report the same value. In AWS that host name is the address the edge serves on: a GET for an object present at the configured origin, addressed to it once the distribution is Deployed, is answered 200 with the origin's bytes, and the behavior configured for the matching path pattern is applied (origin path prefixing, cache-key and header forwarding, OAC/OAI origin authorization and its 403 refusals, viewer-protocol policy, invalidation, access logging, origin-group failover, CORS preflight). Recorded behavior only for those viewer-side specifics.

Emulator: the published host is not one of the emulator's own hosts, and the harness refuses to route it rather than leaving the emulator: 'endpoint_url was given https://<id>.cloudfront.net/<path> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener' (simparity.published.PublishedUrlRejected). That message's fixed text is the requirement being violated; the host label, port and path are per-run values. All 43 checks fail at the first fetch through the distribution, including the negative ones that need the edge to refuse with a status.

Why: a client deploying an SPA, a static site or an API behind CloudFront gets a domain it cannot reach, so neither the positive paths (content served, invalidation refreshing, headers forwarded to the API) nor the negative ones (a missing OAC bucket policy producing 403 at the edge, a wrong origin path producing 404) can be exercised. Deployment tooling that feeds the GetAtt'd DomainName into the next step — a Route 53 alias, a CNAME, a smoke test — breaks on the published value.

Fix: the CloudFront distribution state/serializer that mints the published domain name, plus the edge listener it must name: publish a host the emulator accepts viewer connections on (one of its own hosts, or a per-resource listener for the distribution) and route viewer requests on it through the cache-behavior/origin-fetch pipeline for S3 and custom (API Gateway) origins alike.

Traces: the 43 failing check ids in trace_ids; representative ones are apigateway__cloudfront__s3.cloudfront-apigw-origin-positive, apigateway__cloudfront__s3.cloudfront-s3-oac-origin-access-positive and apigateway__cloudfront__s3.cli-s3-sync-cloudfront-invalidate-workflow.

## Evidence
Expected: the DomainName reported by CreateDistribution / the CFN stack output / the CLI is a host the emulator serves, and an HTTP(S) GET to https://<DomainName>/<key> returns the origin's response (200 with the object's bytes on the positive checks; the edge's 403/404 refusal on the negative ones).

Actual (all 43 checks, same exception type and message shape):
  simparity.published.PublishedUrlRejected: endpoint_url was given https://d0lc535g268ho1.cloudfront.net/ (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:45823/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
    (apigateway__cloudfront__s3.cfn-deploy-cloudfront-apigw-stack-positive)
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net/index.html ... needs http(s)://127.0.0.1:34673/<resource path> ...
    (apigateway__cloudfront__s3.cfn-deploy-cloudfront-s3-oac-stack-positive)
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dzgeitdq1hlfy0.cloudfront.net/protected.txt ... needs http(s)://127.0.0.1:40317/<resource path> ...
    (apigateway__cloudfront__s3.cloudfront-s3-signed-url-restricted-access-positive)

Host labels, ports and object paths are per-run values; the invariant divergence is that the published host is in real AWS's cloudfront.net namespace instead of an address this emulator is reached on.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (run_behavioral_suite).
2. Replay apigateway__cloudfront__s3.cloudfront-s3-oac-origin-access-positive: create a bucket and an OAC-protected distribution over it, wait for Deployed, read Distribution.DomainName.
3. Observe DomainName is <id>.cloudfront.net; the GET of /index.txt through it is refused by the harness (PublishedUrlRejected) because that host is not one the emulator serves.
4. Repeat with any other id in trace_ids — CFN-deployed, CLI-driven and API-Gateway-origin variants all fail at the same point.
