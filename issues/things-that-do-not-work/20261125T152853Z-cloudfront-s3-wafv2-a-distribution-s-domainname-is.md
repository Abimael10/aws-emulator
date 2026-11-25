Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__s3__wafv2.web-acl-blocks-at-the-edge-for-a-private-s3-origin
Issue-Key: cloudfront__s3__wafv2.web-acl-blocks-at-the-edge-for-a-private-s3-origin|run#12|PublishedUrlRejected|url, route_headers = ctx.endpoint_route(f"{dist['DomainName']}/{key}")
Issue-Commit: 22c4b44b7cd9db0fd90c779e90086560248a3104

# cloudfront__s3__wafv2: a distribution's DomainName is published as a <id>.cloudfront.net host the emulator does not serve, so the web-ACL-at-the-edge check never reaches the edge

> _Note: this issue was identified against the code base at commit `22c4b44b7cd9db0fd90c779e90086560248a3104`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: CreateDistribution publishes Distribution.DomainName as a real-AWS-form CloudFront hostname that is not one of the addresses the emulator answers on, so no viewer request can ever be routed to the edge and an associated CLOUDFRONT-scoped web ACL can never be observed allowing or blocking a request.

AWS: Smithy (com-amazonaws-cloudfront-cloudfront2020-05-31) documents Distribution.DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net." On AWS that announced name is the distribution's live edge address: with a CLOUDFRONT-scoped wafv2 WebACL associated (Smithy com-amazonaws-wafv2-awswaf-20190729, scope CLOUDFRONT), a viewer GET to https://<DomainName>/<key> whose request matches a Block rule is answered by the edge with HTTP 403 before the origin is fetched, a non-matching GET is answered 200 with the private S3 origin's bytes, and GetSampledRequests then reports a sample for the rule's metric with Action=BLOCK and the triggering header. The contract that matters is that the name the service hands back is reachable, i.e. the service answers viewer traffic on the address it announces.

Emulator: the distribution's published DomainName is a <generated-id>.cloudfront.net hostname (this run: dgzni4pnd7h2s3.cloudfront.net; the generated id itself is per-run and not a requirement). That host is not an address the emulator serves — the harness refuses to route to it with `simparity.published.PublishedUrlRejected: endpoint_url was given https://<DomainName>/<key> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener`. The emulator announced an address other than the one it is reached on, so neither the 200 nor the 403 leg of the scenario is ever issued and the check gets no verdict.

Note this is a change of signature, not a new scenario: open issue 20261125T052443Z-cloudfront-s3-wafv2-the-web-acl-at-the.md carries this same check id and records the earlier form, where DomainName was published as "127.0.0.1/_aws/cloudfront/<DistributionId>" and the check timed out at its 60 s budget. The announced value now has AWS's shape but is still unreachable, so the scenario is still blocked. Sibling open issues 20261125T150300Z-cloudfront-ec2-elbv2-a-vpc-origin-distribution-publishes.md and 20261125T052443Z-cloudfront-s3-the-distribution-s-published-domainname-resolves.md describe the same unreachable-DomainName shape for other check sets but do not carry this check or its replay key.

Why: a client that reads DomainName and sends viewer traffic to it — the documented and only way to reach a distribution — cannot reach the distribution at all. WAF protection at the edge is unverifiable: a distribution with a Block-rule web ACL associated and one with no web ACL are indistinguishable, so a caller cannot tell protected from unprotected, and CloudFront's whole data plane (cache behavior, OAC-signed origin fetch, custom error responses) is unexercisable through the published name.

Fix: the CloudFront CreateDistribution/UpdateDistribution handlers and the distribution state that mints DomainName must publish an authority the emulator actually listens on (the serving host:port, or a per-resource listener host it claims), and the edge data plane bound to that authority must evaluate an associated CLOUDFRONT-scoped web ACL before the origin fetch and answer 403 on a Block match, 200 with the origin object otherwise, while recording the request for wafv2 GetSampledRequests.

Traces: cloudfront__s3__wafv2.web-acl-blocks-at-the-edge-for-a-private-s3-origin.

## Evidence
Expected (AWS): CreateDistribution returns Distribution.DomainName as a reachable CloudFront edge address (documented example form d111111abcdef8.cloudfront.net). A viewer GET https://<DomainName>/<key> with header x-sim-block: 1 matching the web ACL's Block rule is answered HTTP 403 by the edge, with none of the origin object's bytes in the body; the same GET without that header is answered HTTP 200 carrying the object's bytes; GetSampledRequests for the rule's metric then returns a sample whose Request.URI contains the key, Action == "BLOCK", Request.Headers containing x-sim-block=1, and PopulationSize > 0.

Actual (emulator): DomainName is published as a <generated-id>.cloudfront.net hostname that the emulator does not serve, and routing to it is refused: `simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net/sim2real-cfwaf-<run>/index.json (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:34021/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener` (the host label, object key and port are per-run values; the requirement is that the announced host is one the emulator answers on). No viewer response — neither the 200 nor the 403 — is obtained, so none of the assertions above is reached.

## Steps to Reproduce
1. Create a private S3 bucket (all four public-access blocks on) and put an object at <prefix>/index.json.
2. Create a CloudFront OriginAccessControl (s3, SigningBehavior always, sigv4).
3. In us-east-1, CreateWebACL with Scope=CLOUDFRONT, DefaultAction Allow and one rule whose ByteMatchStatement matches SingleHeader x-sim-block EXACTLY "1" with Action Block.
4. CreateDistribution over the S3 origin with that OAC and WebACLId = the web ACL's ARN; wait for Status == Deployed; grant the distribution read on the bucket via a bucket policy.
5. Read Distribution.DomainName and GET https://<DomainName>/<key> — first without the header, then with x-sim-block: 1.
6. Observe the announced host is a <id>.cloudfront.net name the emulator does not serve, so the request cannot be addressed to the emulator at all and neither the 200 nor the 403 leg is answered.
7. Re-replay the check id in trace_ids to confirm a fix.
