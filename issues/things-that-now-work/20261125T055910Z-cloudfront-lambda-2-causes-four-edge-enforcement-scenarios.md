Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-allowed-methods-post-rejected cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-geo-restriction-blocklist-blocked cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-signed-url-source-ip-denied cloudfront__lambda.origin-access-control-signs-lambda-url-origin-and-direct-call-denied cloudfront__lambda.xsvc-cloudfront-2-lambda-edge-unpublished-version-rejected
Issue-Key: cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-allowed-methods-post-rejected|_poll_until#44|Timeout|time.sleep(interval_s)
Issue-Key: cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-geo-restriction-blocklist-blocked|_poll_until#17|Timeout|time.sleep(interval_s)
Issue-Key: cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-signed-url-source-ip-denied|_poll_until#40|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Key: cloudfront__lambda.origin-access-control-signs-lambda-url-origin-and-direct-call-denied|_poll_until#37|Timeout|time.sleep(interval_s)
Issue-Key: cloudfront__lambda.xsvc-cloudfront-2-lambda-edge-unpublished-version-rejected|_poll_until#42|Timeout|time.sleep(interval_s)
Issue-Commit: 3346f0c8eb008cd7e8905968523e3939c8dd53a9

# cloudfront__lambda: 2 causes — four edge-enforcement scenarios over a Lambda function-URL origin never reach the edge, and the Lambda@Edge $LATEST association refusal never completes

> _Note: this issue was identified against the code base at commit `3346f0c8eb008cd7e8905968523e3939c8dd53a9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Two root causes, both reported from CloudFront-over-Lambda checks that produce no verdict at all.

Cause 1 of 4... (see numbering below; this issue carries 2 causes).

Cause 1 of 2: the edge data plane never answers a viewer request through a distribution over a Lambda function-URL origin, so four edge-enforcement scenarios exhaust their budget with no verdict.
AWS:      Smithy cloudfront CreateDistribution returns Distribution whose required Status member is documented "When the status is Deployed, the distribution's information is fully propagated to all CloudFront edge locations", and DomainName "The distribution's CloudFront domain name". Recorded behavior only for the data plane: the recorded runs complete each of these scenarios inside the suite's 60 s per-check budget, and the edge enforces, before contacting the origin — (a) AllowedMethods: a POST to a behavior whose AllowedMethods are GET/HEAD is refused by the edge with HTTP 403 and the origin is never invoked; (b) Restrictions.GeoRestriction RestrictionType=blacklist: a viewer resolved to a listed country is refused 403 at the edge and the origin is never contacted, while an unlisted viewer is served the origin's answer with the CloudFront-Viewer-Country header; (c) a trusted-key-group signed URL with an IpAddress policy condition: a request from an address outside the policy is refused 403 at the edge while the signed, in-policy request is served; (d) an OriginAccessControl of origin type lambda with SigningBehavior=always: the edge reaches the function URL with a SigV4 signature it generates, while a direct unsigned call to the same function URL is denied.
Emulator: none of the four reaches a graded assertion. Three report only `Error: Check timed out after 60.0s` (no stack frame, no status, no error code, no message); the signed-URL one reports verbatim `TimeoutError: 200 from the unsigned /whoami behavior: the run budget ended first; not graded` — i.e. even the baseline unsigned GET through the edge never returns 200. Open issue 20261125T035244Z-cloudfront-lambda-the-cloudfront-geo-restriction-blocklist-scenario.md describes the geo-restriction check's 60 s timeout; the suite reports that check as not owned, and the other three show the same symptom, so they are filed here as one cause with that issue named.
Why:      A client relying on AWS's behavior gets an unbounded wait where AWS answers in seconds, and four separate security properties of the edge are entirely unverified: an edge that forwarded a disallowed method to the origin, served a blocked country, honoured an out-of-policy signed URL, or let an unsigned direct call through would be indistinguishable from this result.
Fix:      The CloudFront distribution lifecycle (CreateDistribution/GetDistribution reaching a servable Deployed state promptly, at an address the emulator serves) and the edge data plane's request-admission stages — allowed-methods, geo restriction, trusted-key-group signature/IpAddress validation, and OAC SigV4 origin signing — which must run before the origin is contacted. The bare timeouts carry no stack frame, so the stalling call must be found by replaying the listed checks and timing each step.
Traces:   cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-allowed-methods-post-rejected, cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-geo-restriction-blocklist-blocked, cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-signed-url-source-ip-denied, cloudfront__lambda.origin-access-control-signs-lambda-url-origin-and-direct-call-denied.

Cause 2 of 2: the control-plane check that a Lambda@Edge association naming an unpublished version is refused never completes.
AWS:      Smithy cloudfront CreateDistribution models the error InvalidLambdaFunctionAssociation (client, HTTP 400); AWS refuses a LambdaFunctionAssociation whose LambdaFunctionARN is unqualified or names $LATEST (a Lambda@Edge association must name a published version, in us-east-1), and that refusal is immediate. Recorded behavior only for timing: the recorded run completes this accept/refuse pair inside the 60 s budget.
Emulator: the check produces no verdict — `Error: Check timed out after 60.0s` — so neither the refusal of the $LATEST association nor the acceptance of the published-version association is observed. Whether the emulator answers 400 InvalidLambdaFunctionAssociation, accepts the bad association, or hangs cannot be told apart. A previously resolved issue on this surface (20261113T232932Z-cloudfront-lambda-the-lambda-edge-association-lifecycle-reject.md) covered the association lifecycle making no observable progress; this check is again producing nothing.
Why:      A client that mistakenly associates $LATEST gets no modeled refusal it can catch, and a client that associates a correct published version cannot confirm the association took effect; the whole Lambda@Edge admission contract is unverified.
Fix:      The CreateDistribution/UpdateDistribution handler's LambdaFunctionAssociations validation (refuse an unqualified/$LATEST ARN with 400 InvalidLambdaFunctionAssociation, as modeled) and whatever in the distribution lifecycle makes this check exceed its budget.
Traces:   cloudfront__lambda.xsvc-cloudfront-2-lambda-edge-unpublished-version-rejected.

## Evidence
Cause 1 of 2 — four checks, no verdict:
[mismatch] cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-allowed-methods-post-rejected — Error: Check timed out after 60.0s
[mismatch] cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-geo-restriction-blocklist-blocked — Error: Check timed out after 60.0s
[mismatch] cloudfront__lambda.origin-access-control-signs-lambda-url-origin-and-direct-call-denied — Error: Check timed out after 60.0s (the check carries its own DATAPLANE_UNIMPLEMENTED marker; not even that is produced)
[error] cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-signed-url-source-ip-denied —
  File "...deep_cloudfront_2_cf_edge_enforce_signed_url_source_ip_denied.py", line 354, in run -> who = _poll_until(lambda: _get(session, f"{edge}/whoami"), ...)
  TimeoutError: 200 from the unsigned /whoami behavior: the run budget ended first; not graded
Expected (AWS): each scenario completes in seconds — 403 at the edge for the disallowed POST, 403 for the blocklisted viewer country, 403 for the out-of-policy signed URL (and 200 for the in-policy one), 200 through the edge for the OAC-signed function-URL origin with the direct unsigned call denied.
Actual (emulator): no status, body, error code or message is produced in any of the four; three are killed at the 60 s budget and the fourth never gets 200 on its baseline unsigned path.

Cause 2 of 2:
[mismatch] cloudfront__lambda.xsvc-cloudfront-2-lambda-edge-unpublished-version-rejected — Error: Check timed out after 60.0s
Expected (AWS): CreateDistribution/UpdateDistribution with a LambdaFunctionAssociation naming $LATEST (or an unqualified ARN) is refused HTTP 400 InvalidLambdaFunctionAssociation, while the published-version association is accepted; both answers arrive in seconds.
Actual (emulator): the check never completes, so no status or error code is observed at all.

## Steps to Reproduce
Cause 1 of 2:
1. Create a Lambda function that echoes its marker, the method, the raw query string with a per-request nonce, the viewer address/country headers and the header names it saw; create its function URL.
2. CreateOriginAccessControl (origin type lambda, SigningBehavior=always, SigningProtocol=sigv4), a cache policy, and CreateDistribution over the function-URL host; for each variant add (a) AllowedMethods GET/HEAD only, (b) Restrictions.GeoRestriction {RestrictionType: blacklist, Items: [<country>]}, (c) a TrustedKeyGroups behavior fetched with a signed URL carrying an IpAddress policy condition, (d) the OAC alone.
3. Wait for the distribution to be servable and fetch through the edge.
   Expected: (a) 403 for POST with the origin not invoked; (b) 403 for the blocked country, 200 with CloudFront-Viewer-Country otherwise; (c) 403 for the out-of-policy signed URL, 200 for the in-policy one; (d) 200 through the edge and a denial for the direct unsigned function-URL call.
   Observed: the checks are terminated at the 60 s budget; the signed-URL variant never even gets 200 from the unsigned /whoami behavior.

Cause 2 of 2:
1. Create a Lambda@Edge-eligible function in us-east-1 and leave it unpublished ($LATEST), then publish a version.
2. CreateDistribution (or UpdateDistribution) with a LambdaFunctionAssociation whose LambdaFunctionARN is the $LATEST/unqualified ARN.
   Expected: HTTP 400 InvalidLambdaFunctionAssociation, immediately; the published-version association is then accepted.
   Observed: the check never completes inside its 60 s budget, so neither answer is observed.
