Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudfront__dynamodb__lambda__s3.e2e-spa-distribution-serves-shell-and-api-writes-dynamodb
Issue-Key: apigateway__cloudfront__dynamodb__lambda__s3.e2e-spa-distribution-serves-shell-and-api-writes-dynamodb|run#47|AssertionError|assert isinstance(missing, requests.Response) and missing.status_code == 200, (
Issue-Commit: f2831bfd9793e6f7d4548f15bac1de57747f4451

# apigateway__cloudfront__dynamodb__lambda__s3: CustomErrorResponses are never applied at the edge — an origin 403/404 is passed through to the viewer instead of being replaced by the custom error page with its ResponseCode

> _Note: this issue was identified against the code base at commit `f2831bfd9793e6f7d4548f15bac1de57747f4451`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistributionWithTags (DistributionConfig.CustomErrorResponses) / CloudFront viewer GET at the distribution edge

## Description
Cause 1 of 1: a distribution configured with CustomErrorResponses does not substitute the custom error page for an origin error status; the origin's own status is returned to the viewer verbatim.

AWS: Smithy: DistributionConfig.CustomErrorResponses is a list of CustomErrorResponse, whose doc says the type "controls: Whether CloudFront replaces HTTP status codes in the 4xx and 5xx range with custom error messages before returning the response to the viewer". CustomErrorResponse.ErrorCode (required, integer) is "The HTTP status code for which you want to specify a custom error page"; ResponsePagePath is "The path to the custom error page that you want CloudFront to return to a viewer when your origin returns the HTTP status code specified by ErrorCode"; ResponseCode is "The HTTP status code that you want CloudFront to return to the viewer along with the custom error page" — explicitly "You might want to return a 200 status code (OK) and static website so your customers don't know that your website is down." So for a distribution whose CustomErrorResponses map ErrorCode 403 and 404 to ResponsePagePath=/index.html with ResponseCode=200 and ErrorCachingMinTTL=0, a viewer GET of a path whose origin object does not exist must be answered with HTTP status 200 and a body that is the bytes of the object at the ResponsePagePath — not the origin's 403/404 status and not the origin's error document.

Emulator: the viewer GET of a path with an extension that the S3 origin does not hold (an OAC-locked bucket, so a missing key yields 403 from the origin) is answered 403 to the viewer. The replay asserts status 200 and gets `<Response [403]>`. The custom-error-response table is accepted at CreateDistributionWithTags but is never consulted when the origin response status is in the 4xx range, so neither the status substitution (ResponseCode) nor the page substitution (ResponsePagePath) happens. Everything earlier in the same check now works — GET / serves the DefaultRootObject bytes, GET /app.js serves the stored object, and the viewer-request function's extensionless rewrites to /index.html are honoured — so the divergence is isolated to custom error responses.

Why: single-page applications behind CloudFront rely on exactly this rule to turn any unknown deep-link or missing asset into the app shell with a 200, which is the documented and near-universal SPA configuration. A client gets a hard 403/404 (and, with an OAC-locked bucket, an Access Denied page leaking the origin's failure mode) where AWS would have returned the shell with a 200, so the application never boots on those paths and the origin's error surface is exposed to the viewer.

Fix: the CloudFront edge response path — after the origin response is received and before it is returned to the viewer, match the origin's status against the distribution's DistributionConfig.CustomErrorResponses; on a match, fetch ResponsePagePath from the behavior that serves it and return it with ResponseCode as the viewer status, honouring ErrorCachingMinTTL for caching of that error.

Related but NOT covering this: the open 20261110T092303Z-apigateway-cloudfront-dynamodb-lambda-s3-a-distribution-s-c22e80.md describes GET / answering 200 without the DefaultRootObject bytes — that step now passes in this replay and the failure has moved to the custom-error-response substitution. The open 20261117T115022Z-cloudfront-iam-lambda-logs-2-causes-a-negative.md covers CustomErrorResponse.ErrorCachingMinTTL validation and caching duration under a different service token, not the status/page substitution itself.

Traces: apigateway__cloudfront__dynamodb__lambda__s3.e2e-spa-distribution-serves-shell-and-api-writes-dynamodb.

## Evidence
Check: apigateway__cloudfront__dynamodb__lambda__s3.e2e-spa-distribution-serves-shell-and-api-writes-dynamodb [mismatch]

Distribution config under test (from the replayed check): CustomErrorResponses = [{ErrorCode: 403, ResponsePagePath: "/index.html", ResponseCode: "200", ErrorCachingMinTTL: 0}, {ErrorCode: 404, ResponsePagePath: "/index.html", ResponseCode: "200", ErrorCachingMinTTL: 0}], DefaultRootObject=index.html, default behavior targeting an OAC-locked S3 REST-endpoint origin.

Steps that passed in the same run: GET / -> 200; GET /app.js -> 200 with the stored bytes; GET /dashboard and GET /settings/profile -> 200 with the shell (viewer-request function rewrite).

Failing step:
  missing = get("/assets/missing.js")
  assert isinstance(missing, requests.Response) and missing.status_code == 200

Expected: HTTP 200 whose body is the /index.html object's bytes (the custom error page), because the origin's 403 for the absent key matches the ErrorCode 403 entry.
Actual: AssertionError: GET /assets/missing.js: the origin's error becomes the shell with ResponseCode 200, got <Response [403]>

(The 403 here is the origin's status for a key an OAC-locked bucket does not hold; the same divergence applies to the 404 entry. The particular path, bucket name, distribution id and object bytes are the check's values, not part of the requirement — the requirement is that any origin status listed in CustomErrorResponses is replaced by the configured ResponsePagePath body and ResponseCode.)

## Steps to Reproduce
1. Create an S3 bucket and put index.html and app.js at its root. Do NOT create /assets/missing.js.
2. Create an origin access control (type s3, SigningBehavior always, SigningProtocol sigv4).
3. CreateDistributionWithTags with that bucket's REST endpoint as an origin (S3OriginConfig + OriginAccessControlId), DefaultRootObject=index.html, and DistributionConfig.CustomErrorResponses = {Quantity: 2, Items: [{ErrorCode: 403, ResponsePagePath: "/index.html", ResponseCode: "200", ErrorCachingMinTTL: 0}, {ErrorCode: 404, ResponsePagePath: "/index.html", ResponseCode: "200", ErrorCachingMinTTL: 0}]}.
4. PutBucketPolicy allowing s3:GetObject to Principal {"Service": "cloudfront.amazonaws.com"} with Condition StringEquals AWS:SourceArn = the distribution's ARN. Wait for Status=Deployed.
5. GET https://<distribution domain>/assets/missing.js (a key the bucket does not hold, so the origin answers 403).
   -> EXPECT HTTP 200 whose body is the index.html bytes.
   -> OBSERVE HTTP 403 (the origin's status), with no page substitution.
6. Or replay the check id in trace_ids.
