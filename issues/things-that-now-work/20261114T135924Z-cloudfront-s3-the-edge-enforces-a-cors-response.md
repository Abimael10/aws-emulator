Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__s3.response-headers-policy-cors-get-granted-disallowed-method-refused
Issue-Key: cloudfront__s3.response-headers-policy-cors-get-granted-disallowed-method-refused|run#15|AssertionError|assert (resp.status_code, resp.content) == (200, BODY), (
Issue-Commit: 743af1aef2870b34c6c9092ae87b31cfc860bafb

# cloudfront__s3: the edge enforces a CORS response-headers policy server side — a GET carrying a non-allowlisted Origin is refused 403 'cross-site origin' instead of being served the object with no Access-Control-Allow-Origin header

> _Note: this issue was identified against the code base at commit `743af1aef2870b34c6c9092ae87b31cfc860bafb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CloudFront edge data-plane request (GET https://<distribution DomainName>/<key>) on a behavior with a CORS ResponseHeadersPolicy

## Description
Cause 1 of 1: the emulator's edge treats a ResponseHeadersPolicy CorsConfig as an access-control rule and REFUSES a request whose Origin header is not in AccessControlAllowOrigins.
AWS: Smithy: ResponseHeadersPolicyCorsConfig is documented as "A configuration for a set of HTTP response headers that are used for cross-origin resource sharing (CORS). CloudFront ADDS these headers to HTTP responses that it sends", and AccessControlAllowOrigins is "A list of origins (domain names) that CloudFront can use as the value for the Access-Control-Allow-Origin HTTP response header." CORS is a browser-enforced mechanism: the policy only decides which Access-Control-* response headers are emitted, it never gates the request. Recorded behavior: a GET whose Origin matches the allowlist is answered 200 with the object's bytes and exactly Access-Control-Allow-Origin: <that origin>; the SAME GET with a foreign Origin is also answered 200 with the object's bytes, simply carrying no Access-Control-* header at all (the browser, not CloudFront, then blocks the script's access). Only a method outside the cache behavior's AllowedMethods is refused by CloudFront itself, with 403 and the CloudFront error page message "This distribution is not configured to allow the HTTP request method that was used for this request. The distribution supports only cachable requests." plus x-cache: Error from cloudfront.
Emulator: the allowlisted-origin GET is correct (200, object bytes, exactly Access-Control-Allow-Origin), but the identical GET carrying Origin: <a non-allowlisted origin> is refused with HTTP 403 and the body 'cross-site origin'. That status, and that body text, are the emulator's own invention: AWS emits neither for this request class, and 'cross-site origin' is not a CloudFront error-page message (CloudFront's 403 pages are HTML with a <HR noshade size="1px"> rule followed by the message line, and no CloudFront message reads 'cross-site origin'). Because the refusal aborts the check, the subsequent disallowed-method branch (DELETE -> 403 with the AllowedMethods message and x-cache: Error from cloudfront) was never exercised.
Why: the edge blocks traffic AWS serves. Any non-browser client that happens to send an Origin header (a server-side fetch, a proxy, a test harness, a webview) is refused 403 by the emulator while production returns the object; and a web client validating the real contract — that the body is still delivered but the browser denies the script access because Allow-Origin is absent — cannot observe it at all, so CORS misconfiguration is 'caught' offline by a mechanism that does not exist in AWS and missed where it actually matters.
Fix: the edge request/response pipeline's response-headers-policy application step — remove the server-side Origin check: the matched behavior's CorsConfig must only select the Access-Control-* headers written onto the response (Allow-Origin set to the request's Origin when it matches AccessControlAllowOrigins, omitted entirely when it does not), and must never alter the status or body. Request refusal at the edge belongs to the behavior's AllowedMethods / ViewerProtocolPolicy / signed-URL checks, which answer 403 with CloudFront's own error page and x-cache: Error from cloudfront.
Traces: cloudfront__s3.response-headers-policy-cors-get-granted-disallowed-method-refused. No open issue carries this replay key; the neighbouring cloudfront__s3 edge issues (e.g. 20261110T092303Z-cloudfront-s3-6-net-new-causes-in-the.md, 20261110T153718Z-cloudfront-s3-the-oac-oai-granted-s3-origin.md) describe an S3-origin edge that serves nothing or refuses OAC fetches, which is not what is observed here — the allowlisted GET is served correctly from the OAC origin.

## Evidence
Check cloudfront__s3.response-headers-policy-cors-get-granted-disallowed-method-refused:
  AssertionError: CORS is not enforced server side: the foreign GET is still served, got (403, b'cross-site origin')
  (the assertion's own wording states the AWS expectation: (resp.status_code, resp.content) == (200, BODY))
  Request: GET https://<DomainName>/app/config.json with header Origin: https://other.example.net, on a distribution whose default behavior carries a ResponseHeadersPolicy whose AccessControlAllowOrigins lists only https://app.example.com.
  Expected (AWS): HTTP 200, body = the object's bytes, and no Access-Control-* response header.
  Actual (emulator): HTTP 403, body 'cross-site origin'.
  Immediately before, the allowlisted-origin GET (Origin: https://app.example.com) was answered 200 with the object's bytes and exactly {access-control-allow-origin: https://app.example.com}, so the policy is applied and the origin fetch works — the divergence is the added server-side refusal.
  Not reached because of it: DELETE on the same URL must be refused 403 with CloudFront's message "This distribution is not configured to allow the HTTP request method that was used for this request. The distribution supports only cachable requests." and x-cache: Error from cloudfront. The origin hostnames, key name and object bytes are the recording's values filling the template, not requirements.

## Steps to Reproduce
1. Create a private S3 bucket and PutObject Key='app/config.json' with a JSON body.
2. CreateOriginAccessControl (s3 / always / sigv4) and CreateResponseHeadersPolicy with a CorsConfig whose AccessControlAllowOrigins lists exactly one origin (https://app.example.com), AccessControlAllowMethods limited to GET/HEAD.
3. CreateDistribution (us-east-1 control plane) over that bucket with the OAC, the managed CachingDisabled cache policy and that ResponseHeadersPolicyId on the default behavior whose AllowedMethods excludes DELETE; put the bucket policy granting s3:GetObject to cloudfront.amazonaws.com with AWS:SourceArn = the distribution ARN; wait for Status=Deployed.
4. GET https://<DomainName>/app/config.json with Origin: https://app.example.com -> 200, object bytes, Access-Control-Allow-Origin: https://app.example.com (correct today).
5. Re-issue the SAME GET with Origin: https://other.example.net -> the emulator answers 403 'cross-site origin'; AWS answers 200 with the object's bytes and no Access-Control-* header.
6. Then DELETE the same URL with the allowed Origin -> must be 403 with CloudFront's AllowedMethods message and x-cache: Error from cloudfront, and the object must remain readable via S3 GetObject.

Or replay the check id in trace_ids.
