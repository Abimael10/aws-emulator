Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__s3.cache-tag-invalidation-of-an-unmatched-tag-evicts-nothing cloudfront__s3.cache-tag-invalidation-refreshes-the-tagged-object-and-spares-the-untagged-one cloudfront__s3.cf-origin-access-control-blocks-direct-s3-access-rejected cloudfront__s3.cf-s3-large-object-first-fetch-miss-second-fetch-hit cloudfront__s3.cf-s3-response-headers-match-origin-and-via-form cloudfront__s3.cf-viewer-get-with-body-rejected cloudfront__s3.default-root-object-serves-root-but-tenant-directory-rejected cloudfront__s3.e2e-invalidate-cloudfront-cache-lifecycle cloudfront__s3.invalidation-refreshes-once-and-the-edge-re-caches-the-updated-object cloudfront__s3.oac-grant-scoped-to-another-distribution-arn-forbidden cloudfront__s3.oac-origin-tenant-directory-path-rejected-until-list-reveals-no-such-key cloudfront__s3.oac-signed-origin-fetch-without-bucket-policy-grant-forbidden cloudfront__s3.oai-origin-edge-rejects-a-missing-key-with-access-denied cloudfront__s3.response-headers-policy-cors-get-granted-disallowed-method-refused cloudfront__s3.signed-cookies-key-group-admits-private-prefix-rejects-outside-resource cloudfront__s3.viewer-request-function-edge-runs-live-code-until-an-edit-is-published
Issue-Key: cloudfront__s3.cache-tag-invalidation-of-an-unmatched-tag-evicts-nothing|_get#13|PublishedUrlRejected|target, route_headers = ctx.endpoint_route(url)
Issue-Key: cloudfront__s3.cache-tag-invalidation-refreshes-the-tagged-object-and-spares-the-untagged-one|_get#13|PublishedUrlRejected|target, route_headers = ctx.endpoint_route(url)
Issue-Key: cloudfront__s3.cf-origin-access-control-blocks-direct-s3-access-rejected|run#20|PublishedUrlRejected|cf_url, cf_headers = ctx.endpoint_route(f"{domain}/{key}")
Issue-Key: cloudfront__s3.cf-s3-large-object-first-fetch-miss-second-fetch-hit|run#23|PublishedUrlRejected|url, route_headers = ctx.endpoint_route(f"{domain_name}/{key}")
Issue-Key: cloudfront__s3.cf-s3-response-headers-match-origin-and-via-form|run#20|PublishedUrlRejected|url, route_headers = ctx.endpoint_route(f"{domain}/{key}")
Issue-Key: cloudfront__s3.cf-viewer-get-with-body-rejected|run#19|PublishedUrlRejected|url, route_headers = ctx.endpoint_route(f"{domain}/{key}")
Issue-Key: cloudfront__s3.default-root-object-serves-root-but-tenant-directory-rejected|_get#12|PublishedUrlRejected|target, route_headers = ctx.endpoint_route(url)
Issue-Key: cloudfront__s3.e2e-invalidate-cloudfront-cache-lifecycle|phase#8|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Key: cloudfront__s3.invalidation-refreshes-once-and-the-edge-re-caches-the-updated-object|_get#14|PublishedUrlRejected|target, route_headers = ctx.endpoint_route(url)
Issue-Key: cloudfront__s3.oac-grant-scoped-to-another-distribution-arn-forbidden|run#14|PublishedUrlRejected|url, route_headers = ctx.endpoint_route(f"{dist['DomainName']}/{key}")
Issue-Key: cloudfront__s3.oac-origin-tenant-directory-path-rejected-until-list-reveals-no-such-key|_get#10|PublishedUrlRejected|target, route_headers = ctx.endpoint_route(url)
Issue-Key: cloudfront__s3.oac-signed-origin-fetch-without-bucket-policy-grant-forbidden|run#9|PublishedUrlRejected|url, route_headers = ctx.endpoint_route(f"{dist['DomainName']}/{key}")
Issue-Key: cloudfront__s3.oai-origin-edge-rejects-a-missing-key-with-access-denied|_get#10|PublishedUrlRejected|target, route_headers = ctx.endpoint_route(url)
Issue-Key: cloudfront__s3.response-headers-policy-cors-get-granted-disallowed-method-refused|run#10|PublishedUrlRejected|url, route = ctx.endpoint_route(f"{dist['DomainName']}/{KEY}")
Issue-Key: cloudfront__s3.signed-cookies-key-group-admits-private-prefix-rejects-outside-resource|run#31|PublishedUrlRejected|edge, route_headers = ctx.endpoint_route(domain)
Issue-Key: cloudfront__s3.viewer-request-function-edge-runs-live-code-until-an-edit-is-published|_get#12|PublishedUrlRejected|target, route_headers = ctx.endpoint_route(url)
Issue-Commit: a11be7ebac31f84405a5fd36d8a18986542b4650

# cloudfront__s3: the S3-origin edge is unreachable again — the published DomainName is an unserved <id>.cloudfront.net host, refusing all 16 remaining data-plane checks

> _Note: this issue was identified against the code base at commit `a11be7ebac31f84405a5fd36d8a18986542b4650`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
REOPENS 20261109T181319Z-cloudfront-s3-the-s3-origin-edge-is-unreachable.md: its fix left the call refused. That issue reported the S3-origin edge unreachable because the distribution's DomainName was published as an unserved <id>.cloudfront.net host, and the published member is again exactly that, so this is the same defect rather than a new one. The open issue 20261125T052443Z-cloudfront-s3-the-distribution-s-published-domainname-resolves.md covers the same family for an earlier form of the value and carries none of the replay keys of the 16 checks below, so they are filed here and that issue is named as instructed.

Cause 1 of 1: a distribution over an S3 origin publishes Distribution.DomainName as a real-AWS <id>.cloudfront.net host that the emulator does not answer on, so no viewer request reaches the edge.

AWS: Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31) documents Distribution$DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net" — and in AWS that host is the address the edge serves on. Recorded behavior only for the viewer side: a GET addressed to it on a Deployed distribution returns the S3 object's bytes with a Via header of CloudFront's form, applies the cache (first fetch Miss, repeat Hit), honours DefaultRootObject, enforces OAC/OAI origin authorization (a grant scoped to another distribution's ARN or a missing bucket-policy grant yields the S3 403 AccessDenied body at the edge, a missing key yields the AccessDenied/NoSuchKey the bucket policy implies), applies response-headers policies including CORS refusals, enforces signed cookies for a key group, refuses a viewer GET carrying a body, and refreshes exactly the invalidated or cache-tagged objects.

Emulator: the published host is not one of the emulator's own hosts, so the harness refuses to route the viewer request instead of leaving the emulator: 'endpoint_url was given https://<id>.cloudfront.net/<path> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener' (simparity.published.PublishedUrlRejected). The fixed text of that message is the requirement being violated; host label, port and object path are per-run values.

Why: a client that puts a private S3 bucket behind CloudFront cannot fetch anything through the distribution, so neither delivery nor the OAC/OAI authorization boundary nor the cache and invalidation semantics are observable; the single value AWS gives it to address the edge points at real AWS instead of the emulator.

Fix: the CloudFront distribution state/serializer that mints the published domain name, plus the edge listener it must name — publish a host the emulator accepts viewer connections on and serve the S3-origin fetch, OAC/OAI signing and cache pipeline on it.

Traces: the 16 failing check ids in trace_ids; representative ones are cloudfront__s3.cf-s3-large-object-first-fetch-miss-second-fetch-hit, cloudfront__s3.oac-signed-origin-fetch-without-bucket-policy-grant-forbidden and cloudfront__s3.e2e-invalidate-cloudfront-cache-lifecycle.

## Evidence
Expected: an HTTP(S) GET to https://<published DomainName>/<key> on a Deployed distribution reaches the emulator's edge and returns the S3 object (200 + bytes) or the edge's 403/404 refusal on the negative checks.

Actual (all 16 checks, same exception type and message shape):
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net/asset.txt (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:34817/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
    (cloudfront__s3.cf-origin-access-control-blocks-direct-s3-access-rejected)
  AssertionError: [phase 1/7 deploy] endpoint_url was given http://d89fkddmx3r403.cloudfront.net/index.html ... needs http(s)://127.0.0.1:44173/<resource path> ...
    (cloudfront__s3.e2e-invalidate-cloudfront-cache-lifecycle)
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net/onyx-test/index.html ... needs http(s)://127.0.0.1:34125/<resource path> ...
    (cloudfront__s3.oac-signed-origin-fetch-without-bucket-policy-grant-forbidden)

Host labels, ports and keys are per-run values; the invariant divergence is the published host being in real AWS's cloudfront.net namespace rather than an address the emulator is reached on.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (run_behavioral_suite).
2. Replay cloudfront__s3.cf-s3-large-object-first-fetch-miss-second-fetch-hit: create a bucket with an object, create a distribution over it, wait for Deployed, read Distribution.DomainName.
3. Observe DomainName is <id>.cloudfront.net; the viewer GET through it is refused by the harness (PublishedUrlRejected) because that host is not served by the emulator.
4. Repeat with any other id in trace_ids.
