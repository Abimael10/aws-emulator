Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudfront__s3.cloudfront-s3-website-endpoint-origin-positive
Issue-Key: apigateway__cloudfront__s3.cloudfront-s3-website-endpoint-origin-positive|_poll_http_get#19|Timeout|time.sleep(interval_s)
Issue-Commit: 69cb8c4786d799a8a66c2117bf8eebb4fb69db3f

# apigateway__cloudfront__s3: an S3 website-endpoint origin whose bucket publishes its objects under a Referer condition served nothing through the distribution — the anonymous read was decided with no condition context at all

> _Resolution note: this record is the half of
> 20261121T120000Z (two edge data-plane checks) that NOW WORKS, and it claims the one replay key
> above. The other half — `cloudfront-apigw-viewer-request-header-passthrough`, whose
> predicate is not observable from inside the emulator — is still open and is tracked by
> `issues/things-that-do-not-work/20261123T230000Z-apigateway-cloudfront-s3-the-apigw-viewer-request-header-passthrough-check-still-times-out.md`,
> which carries that key, its trace id and the evidence gathered about it._

## Operation
CloudFront viewer GET at a distribution edge (S3 website-endpoint origin) / S3 GetObject on the
website surface

## Description
Cause 1 of 1: a bucket policy statement that publishes a bucket's objects to everyone only under
a CONDITION granted nothing at all to the anonymous caller, so an S3 website-endpoint origin
fronted by a distribution answered every viewer request `403 AccessDenied` and a poll for the
index document never converged.

AWS: a bucket policy is evaluated in the REQUEST's own condition context. `aws:Referer` — "use
this key to compare who referred the request in the client browser with the referer specified in
the policy … The `aws:Referer` request context value is provided by the caller in an HTTP header"
— is therefore answered from the `Referer` the request states, which is exactly how AWS's own
guidance narrows a published site's `s3:GetObject` grant to the front door that states a secret
`Referer` (CloudFront adds it to every origin request through the origin's `CustomHeaders`). A
request stating the value the clause names is served the object; one stating another value, or
none, is refused.

Emulator: `state/refusals.rs::policy_decision_for_anonymous` evaluated the document with no
condition context and no published keys, so EVERY conditioned statement was undecided for the
anonymous caller — a conditioned public-read grant granted nothing, and the website endpoint
refused the read with `403 AccessDenied` for the whole poll.

Fix (landed): the anonymous decision now carries the request's own condition context and declares
the keys it answers (`state/stated_referer.rs` holds the one spelling of `aws:Referer` and the
request-scoped note the routing seam records for every request this service claims;
`state/refusals.rs::request_conditions()` is the one context the store's anonymous and
credentialed decisions share; `service/authorization.rs` answers the same key for the request
gate and publishes it). `aws:SourceIp` stays unanswered — nothing in this emulator attributes a
source address to a request, which the open route53 issue 20261114T080026Z owns.

Traces: apigateway__cloudfront__s3.cloudfront-s3-website-endpoint-origin-positive.

## Evidence
Before the fix, reproduced by `core/router/tests/all/cloudfront_website_endpoint_origin_at_its_published_address.rs`
on the whole assembled emulator, dialled on the address the distribution publishes: the viewer
received `HTTP/1.1 403 Forbidden` with CloudFront's `X-Cache: Error from cloudfront` and the
website endpoint's own page `<html><head><title>403 AccessDenied</title>…<li>Code:
AccessDenied</li><li>Message: Access Denied</li>…`, where the site's index document was expected.
After it, the same dial is answered 200 with the index document, and the same site refuses a
distribution whose origin states another `Referer` or none.

What was already right, and stayed so: the edge serves the index document over both published
spellings of the website host, under `http-only` and `match-viewer`, for the root, a nested
directory, an object key and a key the bucket does not hold, in process and dialled, with the
front door verifying requests
(`services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/tests/all/edge_website_endpoint_origin.rs`
and `.../edge_website_origin_under_request_authentication.rs`). The viewer's own `Authorization`
remains the one header this emulator withholds from every origin, by design.

## Steps to Reproduce
1. Create a bucket, lift its block-public-access settings, and put a bucket policy allowing
   `s3:GetObject` to `"Principal":"*"` with `"Condition":{"StringEquals":{"aws:Referer":"<secret>"}}`.
2. Put an index document and a website configuration naming its suffix.
3. `CreateDistribution` over `<bucket>.s3-website-<Region>.amazonaws.com` as a CUSTOM origin with
   `OriginProtocolPolicy=http-only` and an origin `CustomHeaders` entry `Referer: <secret>`.
4. Wait for `Status=Deployed` and GET `/` at the distribution's published address: AWS serves the
   index document; before the fix the emulator answered 403 AccessDenied for the whole poll.
