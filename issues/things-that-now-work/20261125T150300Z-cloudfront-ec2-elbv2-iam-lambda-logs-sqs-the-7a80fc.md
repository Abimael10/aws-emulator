Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__ec2__elbv2__iam__lambda__logs__sqs.alb-lambda-target-sees-viewer-headers-and-cache-hit-skips-invocation
Issue-Key: cloudfront__ec2__elbv2__iam__lambda__logs__sqs.alb-lambda-target-sees-viewer-headers-and-cache-hit-skips-invocation|_get#40|PublishedUrlRejected|target, route = ctx.endpoint_route(url)
Issue-Commit: 0970c67cd532b68a1462dc1d98bdb119ad45d0ac

# cloudfront__ec2__elbv2__iam__lambda__logs__sqs: the distribution's published DomainName is a cloudfront.net host the emulator does not serve, so the ALB/Lambda-target edge scenario reaches no verdict

> _Note: this issue was identified against the code base at commit `0970c67cd532b68a1462dc1d98bdb119ad45d0ac`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: the distribution announces a DomainName on the public `cloudfront.net` suffix rather than an authority the emulator binds, so the warm-up and viewer requests that drive the ALB -> Lambda-target leg can never be issued and the cache-hit/invocation-count differential is ungradable.

AWS: Smithy models `Distribution$DomainName` on CreateDistribution/GetDistribution output as the domain name corresponding to the distribution. On real AWS that host resolves, terminates TLS and forwards viewer requests to the configured origin; with an origin request policy that names them, the origin sees `CloudFront-Viewer-Address` and `CloudFront-Viewer-Country` plus an `X-Forwarded-For` chain, and a cache hit serves the object without re-invoking the Lambda target. The model does not state where the host lives, so this is recorded behavior only for the locality rule: an address the emulator announces must be one a client of the emulator can reach.

Emulator: the distribution reports `<distribution-id>.cloudfront.net` and nothing answers there. The replay refuses the announced address: `simparity.published.PublishedUrlRejected: endpoint_url was given https://<distribution-id>.cloudfront.net/sim2real/warmup (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener`. The id and port vary per run; only the authority rule is the requirement.

Why: a client following the documented flow (create distribution, wait for Deployed, fetch `https://<DomainName>/<path>`) gets no connection, so it can observe neither the viewer headers CloudFront is supposed to add at the edge nor the fact that a cache hit skips the Lambda invocation. The control plane looks correct while nothing is served.

Fix: the CloudFront distribution serializer that fills `Distribution$DomainName`, together with the per-distribution edge listener it must announce and the viewer-request forwarding that listener should perform into the ALB origin.

Traces: check cloudfront__ec2__elbv2__iam__lambda__logs__sqs.alb-lambda-target-sees-viewer-headers-and-cache-hit-skips-invocation. The open issue 20261125T091314Z-cloudfront-2-net-new-causes-a-distribution-s.md reports a similar DomainName/edge symptom for the bare `cloudfront` heading, but the replay did not mark this check as owned by it.

## Evidence
Expected: after the distribution is Deployed, `GET https://<DomainName>/sim2real/warmup` succeeds through the emulator's own edge listener, the Lambda target behind the ALB observes the CloudFront-Viewer-* headers named by the origin request policy and an X-Forwarded-For chain of IPv4 addresses, and a second identical GET is served from cache without a further Lambda invocation.

Actual: no request is possible. `simparity.published.PublishedUrlRejected: endpoint_url was given https://d89fkddmx3r403.cloudfront.net/sim2real/warmup (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:38183/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener` (id and port are per-run values). Verdict: mismatch.

## Steps to Reproduce
1. Build a VPC, an internal/internet-facing ALB with a Lambda target group, and a listener rule that requires the X-Origin-Secret header.
2. Create a CloudFront distribution over that ALB origin with an origin request policy that names CloudFront-Viewer-Address and CloudFront-Viewer-Country and an origin custom header carrying the secret.
3. Wait for the distribution to reach Deployed and read Distribution.DomainName.
4. Issue `GET https://<DomainName>/sim2real/warmup`.
5. Observe the announced DomainName is a cloudfront.net host the emulator does not listen on, so the viewer request cannot be made at all.
