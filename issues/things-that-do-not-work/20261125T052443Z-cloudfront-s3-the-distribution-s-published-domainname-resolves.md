Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__s3.cache-tag-invalidation-of-an-unmatched-tag-evicts-nothing cloudfront__s3.cache-tag-invalidation-refreshes-the-tagged-object-and-spares-the-untagged-one cloudfront__s3.cf-origin-access-control-blocks-direct-s3-access-rejected cloudfront__s3.cf-s3-large-object-first-fetch-miss-second-fetch-hit cloudfront__s3.cf-s3-response-headers-match-origin-and-via-form cloudfront__s3.cf-viewer-get-with-body-rejected cloudfront__s3.default-root-object-serves-root-but-tenant-directory-rejected cloudfront__s3.e2e-invalidate-cloudfront-cache-lifecycle cloudfront__s3.invalidation-refreshes-once-and-the-edge-re-caches-the-updated-object cloudfront__s3.oac-grant-scoped-to-another-distribution-arn-forbidden cloudfront__s3.oac-origin-tenant-directory-path-rejected-until-list-reveals-no-such-key cloudfront__s3.oac-signed-origin-fetch-without-bucket-policy-grant-forbidden cloudfront__s3.oai-origin-edge-rejects-a-missing-key-with-access-denied cloudfront__s3.response-headers-policy-cors-get-granted-disallowed-method-refused cloudfront__s3.signed-cookies-key-group-admits-private-prefix-rejects-outside-resource cloudfront__s3.viewer-request-function-edge-runs-live-code-until-an-edit-is-published
Issue-Key: cloudfront__s3.cache-tag-invalidation-of-an-unmatched-tag-evicts-nothing|run#13|AssertionError|assert (status, body) == (200, _V1), (
Issue-Key: cloudfront__s3.cache-tag-invalidation-refreshes-the-tagged-object-and-spares-the-untagged-one|run#13|AssertionError|assert (status, body) == (200, _V1), (
Issue-Key: cloudfront__s3.cf-origin-access-control-blocks-direct-s3-access-rejected|_poll_until#23|Timeout|time.sleep(interval_s)
Issue-Key: cloudfront__s3.cf-s3-large-object-first-fetch-miss-second-fetch-hit|_poll#25|Timeout|time.sleep(interval_s)
Issue-Key: cloudfront__s3.cf-s3-response-headers-match-origin-and-via-form|_poll_until#22|Timeout|time.sleep(interval_s)
Issue-Key: cloudfront__s3.cf-viewer-get-with-body-rejected|_poll_until#21|Timeout|time.sleep(interval_s)
Issue-Key: cloudfront__s3.default-root-object-serves-root-but-tenant-directory-rejected|_get_until#12|Timeout|time.sleep(5)
Issue-Key: cloudfront__s3.e2e-invalidate-cloudfront-cache-lifecycle|_get_until#8|Timeout|time.sleep(5)
Issue-Key: cloudfront__s3.invalidation-refreshes-once-and-the-edge-re-caches-the-updated-object|_get_until#14|Timeout|time.sleep(5)
Issue-Key: cloudfront__s3.oac-grant-scoped-to-another-distribution-arn-forbidden|_poll_until#15|Timeout|time.sleep(interval_s)
Issue-Key: cloudfront__s3.oac-origin-tenant-directory-path-rejected-until-list-reveals-no-such-key|_get_until#10|Timeout|time.sleep(5)
Issue-Key: cloudfront__s3.oac-signed-origin-fetch-without-bucket-policy-grant-forbidden|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: cloudfront__s3.oai-origin-edge-rejects-a-missing-key-with-access-denied|_get_until#10|Timeout|time.sleep(5)
Issue-Key: cloudfront__s3.response-headers-policy-cors-get-granted-disallowed-method-refused|_poll#11|AssertionError|raise AssertionError(f"{description} not answered {want} within {timeout_s}s; last {detail!r}")
Issue-Key: cloudfront__s3.signed-cookies-key-group-admits-private-prefix-rejects-outside-resource|_poll#42|Timeout|time.sleep(interval_s)
Issue-Key: cloudfront__s3.viewer-request-function-edge-runs-live-code-until-an-edit-is-published|_get_until#12|Timeout|time.sleep(5)
Issue-Commit: 3ce962b3772a5f276bd697ccb3d5b119b45b10c0

# cloudfront__s3: the distribution's published DomainName resolves to no listener, so 16 edge data-plane checks are refused at connect or time out

> _Note: this issue was identified against the code base at commit `3ce962b3772a5f276bd697ccb3d5b119b45b10c0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: the edge address a distribution publishes serves nothing, so no viewer request through the distribution is ever answered.

AWS: Smithy (com-amazonaws-cloudfront-cloudfront2020-05-31) documents Distribution.DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net." — a bare DNS host name. Once the distribution is Deployed, an HTTPS GET to https://<DomainName>/<path> is answered by the edge: 200 with the origin object's bytes (with CloudFront's response headers, including Via and the cache-status header) for a granted request, and the edge's own refusal for a denied one — 403 AccessDenied when the bucket policy does not grant the OAC/OAI principal, 403 AccessDenied through an OAI origin for a missing key, 404 NoSuchKey through an OAC origin once the key is absent. Invalidation, cache-tag invalidation, default-root-object resolution, response-headers policies and signed cookies are all observable only through that served edge.

Emulator: the DomainName published by CreateDistribution is "127.0.0.1/_aws/cloudfront/<DistributionId>" — a value carrying a URI path rather than a host name — and the host:port it implies (127.0.0.1:443) has no listener, while the control plane answers on a different authority. Every viewer request is therefore refused at connect: the first GET of a cached object returns "(None, 'URLError: <urlopen error [Errno 111] Connection refused>')" where (200, <payload>) was expected, and a poll for a 200 reports "GET https://127.0.0.1/_aws/cloudfront/<Id>/<path> ... not answered 200 within <N>s; last ConnectionError(MaxRetryError('HTTPSConnectionPool(host=\"127.0.0.1\", port=443): Max retries exceeded ... [Errno 111] Connection refused'))". Checks that only poll report "Check timed out after 60.0s" with no verdict. The emulator announced an address other than the one it is reached on.

Why: a client that reads DomainName and addresses the viewer there — the documented way to use a distribution — gets a connection refusal instead of content or a refusal. Both halves of every edge contract collapse: a granted fetch never yields bytes, and a denied fetch never yields AWS's 403/404 with its error code, so a caller cannot distinguish "allowed" from "forbidden" from "edge absent". Cache semantics (miss then hit, invalidation refreshing once, cache-tag invalidation sparing an untagged object), the Via/response-header surface, default-root-object resolution, signed cookies and response-headers-policy CORS enforcement are all unobservable.

Fix: the CloudFront CreateDistribution/UpdateDistribution handler and the distribution state must publish a DomainName that is a bare host authority the emulator serves (no path component), and the edge data plane bound to that authority must resolve the request against the distribution's origin and answer it — including the edge's own refusals with AWS's status and error code. GetDistribution/ListDistributions readback must echo the same value.

Note: open issues 20261029T175823Z-cloudfront-s3-an-oac-protected-s3-origin-never.md, 20261029T175823Z-cloudfront-10-more-divergences-the-edge-data-plane.md, 20261117T143723Z-cloudfront-s3-6-net-new-causes-a-distribution.md and 20261118T131652Z-cloudfront-s3-the-invalidate-once-re-cache-sequence.md describe the same area but carry none of the replay keys below.

Traces: the 16 failing checks in trace_ids, all in the cloudfront__s3 group.

## Evidence
Expected (AWS): https://<DomainName>/<key> answers 200 with the origin bytes for a granted viewer request (e.g. first GET of object A returns (200, '<payload v1>')), and answers the edge's refusal — 403 AccessDenied or 404 NoSuchKey — for a denied one.

Actual (emulator):
  - "AssertionError: first GET of object A returned (None, 'URLError: <urlopen error [Errno 111] Connection refused>'), expected (200, '<payload v1>')"
  - "AssertionError: GET https://127.0.0.1/_aws/cloudfront/<Id>/app/config.json from https://app.example.com not answered 200 within <N>s; last ConnectionError(MaxRetryError('HTTPSConnectionPool(host=\"127.0.0.1\", port=443): Max retries exceeded with url: /_aws/cloudfront/<Id>/app/config.json (Caused by NewConnectionError(\"... [Errno 111] Connection refused\"))'))"
  - the remaining checks: "Error: Check timed out after 60.0s" — no verdict.
  - replayed CreateDistribution payload: 'DomainName': '127.0.0.1/_aws/cloudfront/<DistributionId>' (fixed text is the shape "<host>/_aws/cloudfront/<id>"; the host, port, id and payload strings are per-run values).

## Steps to Reproduce
1. Create a bucket, put two objects in it, and create a distribution over that origin (OAC or OAI).
2. Read Distribution.DomainName — it is "127.0.0.1/_aws/cloudfront/<Id>", not a DNS host name.
3. GET https://<DomainName>/<key>.
4. Observe [Errno 111] Connection refused instead of a 200 with the object bytes (or instead of the edge's 403/404 for the denied variants).
5. Re-replay the check ids in trace_ids to confirm.
