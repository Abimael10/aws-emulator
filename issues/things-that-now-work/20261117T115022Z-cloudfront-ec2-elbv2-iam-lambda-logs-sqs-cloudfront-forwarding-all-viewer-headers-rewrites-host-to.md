Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__ec2__elbv2__iam__lambda__logs__sqs.alb-lambda-target-sees-viewer-headers-and-cache-hit-skips-invocation
Issue-Key: cloudfront__ec2__elbv2__iam__lambda__logs__sqs.alb-lambda-target-sees-viewer-headers-and-cache-hit-skips-invocation|run#49|AssertionError|assert first_headers["host"] == dist_domain, (
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# CloudFront forwarding all viewer headers rewrites Host to the origin's own host instead of passing the viewer's Host through

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: the edge does not preserve the viewer's Host header on an origin request when the cache behavior forwards all viewer headers.
AWS: recorded behavior only — the Smithy cloudfront model describes the configuration (CachePolicy / OriginRequestPolicy header behavior), not the edge. The documented contract the scenario relies on is that a cache behavior configured to forward ALL viewer headers sends the viewer's own Host — the distribution's domain name as the viewer addressed it — to the origin unchanged, which is exactly how an ALB/Lambda origin can tell which front door a request came through.
Emulator: the origin (an ALB Lambda target, which echoes the headers it received) sees Host set to the ORIGIN's own host:authority, not the distribution's domain the viewer used. The header is rewritten on the way to the origin even though the behavior forwards all viewer headers.
Why: any origin that branches on Host — virtual hosting, multi-tenant routing, absolute-URL/redirect generation, signed-URL validation — behaves differently behind the emulator's edge than behind CloudFront. The specific host:port values differ in every environment and are not the requirement; the requirement is that the value the origin sees is the one the viewer sent.
Fix: the CloudFront edge's origin-request construction — the viewer-header forwarding stage must not overwrite Host when the behavior forwards all viewer headers.
Failing check: cloudfront__ec2__elbv2__iam__lambda__logs__sqs.alb-lambda-target-sees-viewer-headers-and-cache-hit-skips-invocation

## Evidence
[mismatch] cloudfront__ec2__elbv2__iam__lambda__logs__sqs.alb-lambda-target-sees-viewer-headers-and-cache-hit-skips-invocation — AssertionError: 'forwarding all viewer headers must carry the viewer's own Host (the distribution's domain) to the origin unchanged, got <origin authority>, expected <distribution domain>' (both rendered as host:port in this environment; the divergence is which of the two the origin received, not the literal addresses).

## Steps to Reproduce
1. Create an ALB with a Lambda target that echoes its received headers, and a CloudFront distribution whose default cache behavior forwards all viewer headers to that ALB origin.
2. GET the distribution's own domain and read the echoed 'host' header from the Lambda target's response: it must be the distribution domain the viewer addressed, not the origin's host.
