Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__s3__wafv2.web-acl-blocks-at-the-edge-for-a-private-s3-origin
Issue-Key: cloudfront__s3__wafv2.web-acl-blocks-at-the-edge-for-a-private-s3-origin|_poll_until#13|Timeout|time.sleep(interval_s)
Issue-Commit: 3ce962b3772a5f276bd697ccb3d5b119b45b10c0

# cloudfront__s3__wafv2: the web-ACL-at-the-edge check reaches no verdict because the distribution's published edge address serves nothing

> _Note: this issue was identified against the code base at commit `3ce962b3772a5f276bd697ccb3d5b119b45b10c0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: no viewer request reaches the edge, so an associated CLOUDFRONT-scoped web ACL can never be observed blocking or allowing a request.

AWS: Smithy (com-amazonaws-cloudfront-cloudfront2020-05-31) documents Distribution.DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net." With a CLOUDFRONT-scoped WebACL associated to the distribution, a viewer GET to https://<DomainName>/<key> that matches a Block rule is answered by the edge with HTTP 403 and WAF's block response body, while a non-matching request is served the private S3 origin's object with 200.

Emulator: the published DomainName is "127.0.0.1/_aws/cloudfront/<DistributionId>", a value that is not a DNS host and whose implied 127.0.0.1:443 has no listener (the control plane answers on a different authority). The check cannot obtain either response and reports "Error: Check timed out after 60.0s" — the whole budget spent with no verdict.

Why: a client cannot verify that WAF protection is in force. Neither the blocked 403 nor the allowed 200 is observable, so an unprotected distribution and a protected one are indistinguishable.

Fix: the CloudFront CreateDistribution/UpdateDistribution handler and distribution state must publish a served host authority as DomainName, and the edge data plane for that authority must evaluate the associated web ACL before the origin fetch and answer accordingly.

Note: open issues 20261110T105638Z-cloudfront-4-net-new-causes-origin-group-failover.md and 20261121T003208Z-cloudfront-2-net-new-causes-an-associated-cloudfront.md describe web-ACL enforcement gaps but carry neither this check nor its replay key.

Traces: the failing check in trace_ids.

## Evidence
Expected (AWS): with a CLOUDFRONT web ACL associated, a matching viewer GET through https://<DomainName>/<key> is answered 403 by the edge (WAF block), and a non-matching GET is answered 200 with the private origin object's bytes.

Actual (emulator): "Error: Check timed out after 60.0s" — the distribution's published DomainName is "127.0.0.1/_aws/cloudfront/<Id>" and the host:port it implies refuses every connection, so neither response is ever obtained.

## Steps to Reproduce
1. Create a private S3 bucket with an object and a distribution over it with OAC.
2. Create a CLOUDFRONT-scoped wafv2 WebACL with a Block rule and associate it to the distribution.
3. Read Distribution.DomainName and GET https://<DomainName>/<key> with a request that matches the Block rule, then with one that does not.
4. Observe neither request is answered — the connect to the announced address is refused and the check exhausts its 60 s budget.
5. Re-replay the check id in trace_ids to confirm.
