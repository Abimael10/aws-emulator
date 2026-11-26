Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3.cors-expose-headers-names-only-the-listed-metadata-header s3.cors-presigned-get-from-the-allowed-origin-without-its-port-rejected s3.cors-presigned-head-carries-the-rule-headers-on-a-hit-and-on-a-miss
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3.cors-expose-headers-names-only-the-listed-metadata-header|_await_cors_grant#6|AssertionError|raise AssertionError(
Issue-Key: s3.cors-presigned-get-from-the-allowed-origin-without-its-port-rejected|_await_cors_grant#6|AssertionError|raise AssertionError(
Issue-Key: s3.cors-presigned-head-carries-the-rule-headers-on-a-hit-and-on-a-miss|_await_cors_grant#5|AssertionError|raise AssertionError(
Issue-Commit: def798b9c9a5a532a0598d5d900881d6a843c290

# s3: a presigned GET/HEAD that carries an Origin allowed by the bucket's CORS configuration is answered 403 with no Access-Control-Allow-Origin, so ExposeHeaders, port-sensitive origin matching and HEAD rule echoing are all unobservable

> _Note: this issue was identified against the code base at commit `def798b9c9a5a532a0598d5d900881d6a843c290`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetObject

## Description
1 cause, grounded in 3 failing checks.

Cause 1 of 1: a correctly signed query-string (presigned) GET or HEAD sent with an `Origin` header to a bucket whose CORSConfiguration lists that origin is refused with HTTP 403 and carries no CORS response headers at all, instead of serving the object with the rule's headers.

AWS: Smithy (services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json) models `CORSRule` with required `AllowedMethods` ("An HTTP method that you allow the origin to execute. Valid values are GET, PUT, HEAD, POST, and DELETE") and required `AllowedOrigins` ("One or more origins you want customers to be able to access the bucket from"), plus `ExposeHeaders` ("One or more headers in the response that you want customers to be able to access from their applications") and `MaxAgeSeconds`. Per AWS's documented CORS behavior, an actual (non-preflight) request whose method is in `AllowedMethods` and whose `Origin` matches an `AllowedOrigins` entry is served normally — HTTP 200 for a hit — and the response carries `Access-Control-Allow-Origin` echoing the request's `Origin` (or `*` for a wildcard rule) together with `Access-Control-Expose-Headers` naming exactly the rule's `ExposeHeaders` entries. Origin matching is on the full origin triple, so `http://www.example.com` does NOT match a rule listing `http://www.example.com:8080`; the listed origin itself must still be allowed. A HEAD is governed by the same rule as a GET and carries the same headers on a hit and on a miss.

Emulator: all three requests are refused `HTTP 403` and the response carries no `Access-Control-Allow-Origin` header (the replay reports `Allow-Origin None`), for 20 s of retries, both for GET and for HEAD, and both for a wildcard-free origin rule and a port-bearing origin rule. Because the request never reaches a 200, neither the `ExposeHeaders` projection nor the port-sensitivity of origin matching nor the HEAD-hit/HEAD-miss header symmetry can be observed. Hypothesis: the same presigned-request refusal path that this report shows for the sibling s3 CORS checks pre-empts CORS evaluation — those siblings return the body `<Error><Code>InvalidAccessKeyId</Code><Message>The AWS Access Key Id you provided does not exist in our records.</Message>...</Error>` with 403 — i.e. the query-string SigV4 credential is rejected before any bucket-CORS handling runs. The recordings for these three checks state only the 403 status and the missing header.

Why: a browser page cannot read a presigned S3 object it is explicitly allowed to read. The fetch fails the same-origin check (no `Access-Control-Allow-Origin`), so JavaScript sees a network error rather than the bytes; `ExposeHeaders` metadata the application depends on is never visible; and a client cannot distinguish "origin not allowed" from "credential rejected", because both come back as a bare 403. Negative CORS expectations become untestable too: a rule that should NOT match (the same host without its port) is indistinguishable from one that should.

Fix: the GetObject/HeadObject request path — the presigned (query-string SigV4) authentication step must admit a URL the emulator itself minted before the bucket-CORS stage, and the S3 edge's CORS stage must then evaluate the bucket's CORSConfiguration for actual requests and emit `Access-Control-Allow-Origin` / `Access-Control-Expose-Headers` on the response, including on refusals for a CORS-configured bucket. Related open issue 20261117T213222Z-s3-3-net-new-browser-edge-refusal-causes.md covers other checks in the same area; the replay suite does not report it as owning these three, which are filed here.

Traces: s3.cors-expose-headers-names-only-the-listed-metadata-header; s3.cors-presigned-get-from-the-allowed-origin-without-its-port-rejected; s3.cors-presigned-head-carries-the-rule-headers-on-a-hit-and-on-a-miss.

## Evidence
Expected (AWS): HTTP 200 with `Access-Control-Allow-Origin: <the request's Origin>` (and `Access-Control-Expose-Headers` listing exactly the rule's ExposeHeaders) for a GET/HEAD whose method and Origin are allowed by a CORSRule.

Actual (emulator):
  [mismatch] s3.cors-expose-headers-names-only-the-listed-metadata-header
    AssertionError: after 20s the GET answered 403 with Allow-Origin None, expected 'https://reader.example.org'
  [mismatch] s3.cors-presigned-get-from-the-allowed-origin-without-its-port-rejected
    AssertionError: after 20s the listed origin's GET answered 403 with Allow-Origin None, expected 'http://www.example.com:8080'
  [mismatch] s3.cors-presigned-head-carries-the-rule-headers-on-a-hit-and-on-a-miss
    AssertionError: after 20s the HEAD answered 403 with Allow-Origin None, expected 'https://viewer.example.net'

The origins above are the values each check configured in its CORSRule — they fill the template `Access-Control-Allow-Origin: <the Origin of the request, when it matches an AllowedOrigins entry>`; the divergence is the 403 status and the absent header, not any particular origin string.

Hypothesis (same report, sibling s3 CORS check s3.cors-first-matching-rule-governs-presigned-get, already owned): the listed origin's presigned GET answered 403 with body
  <Error><Code>InvalidAccessKeyId</Code><Message>The AWS Access Key Id you provided does not exist in our records.</Message>...</Error>
suggesting the presigned credential is rejected before CORS evaluation.

## Steps to Reproduce
1. CreateBucket, PutObject a key, and PutBucketCors with a rule whose AllowedMethods include GET and HEAD, whose AllowedOrigins list one concrete origin (e.g. with an explicit port), and whose ExposeHeaders name one metadata header.
2. Mint a presigned GET (and a presigned HEAD) URL for that key.
3. Fetch each URL with the `Origin` request header set to the listed origin.
4. Observe HTTP 403 and no Access-Control-Allow-Origin / Access-Control-Expose-Headers on the response, instead of 200 with the rule's headers.
5. Replay checks: s3.cors-expose-headers-names-only-the-listed-metadata-header, s3.cors-presigned-get-from-the-allowed-origin-without-its-port-rejected, s3.cors-presigned-head-carries-the-rule-headers-on-a-hit-and-on-a-miss
