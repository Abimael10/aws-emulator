Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge
Issue-Key: cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge|run#37|PublishedUrlRejected|edge, route = ctx.endpoint_route(dist["DomainName"])
Issue-Commit: 0970c67cd532b68a1462dc1d98bdb119ad45d0ac

# cloudfront__ec2__elbv2: a VPC-origin distribution publishes DomainName as a <id>.cloudfront.net host the emulator does not answer on, so the edge check reaches no verdict

> _Note: this issue was identified against the code base at commit `0970c67cd532b68a1462dc1d98bdb119ad45d0ac`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: the distribution's published DomainName is an AWS-shaped `<id>.cloudfront.net` name instead of an address this emulator serves, so no viewer request can ever reach the edge and the whole VPC-origin data-plane scenario is ungradable.

AWS: Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31) models `Distribution$DomainName` on CreateDistribution/GetDistribution output as "the domain name that corresponds to the distribution" — on real AWS that name is a live DNS host that terminates TLS and serves viewer traffic, and a VPC origin pointed at an internal ALB is fetched by the edge on the viewer's behalf (origin-added `X-Origin-Secret` present, so the ALB listener rule serves the marker; absent, so the listener's fixed 403 body is served). The modelled contract says nothing about hosting, so this is recorded behavior only for the locality rule: whatever address the emulator announces must be an address a client of the emulator can dial.

Emulator: the distribution reports a DomainName on the public cloudfront.net suffix and the emulator answers nothing there. The replay refuses the announced address outright: `simparity.published.PublishedUrlRejected: endpoint_url was given https://<distribution-id>.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener`. The `<distribution-id>` and the port are per-run values; the requirement is only that the announced authority be one the emulator listens on.

Why: a client that does what AWS documents — create the distribution, wait for Deployed, then GET `https://<DomainName>/<path>` — gets no connection at all. Every downstream assertion (the marker served through the VPC origin, the secret-header gate, the internal-ALB fetch) is unreachable, so the emulator silently reports a healthy control plane with no data plane behind it.

Fix: the CloudFront distribution read/serialize path that fills `Distribution$DomainName` (and the per-distribution edge listener it should announce) in the cloudfront service crate — DomainName must name an authority the emulator binds, and that authority must proxy viewer requests to the configured origin.

Traces: check cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge. Note: the open issue 20261125T091314Z-cloudfront-ec2-elbv2-a-vpc-origin-distribution-over.md describes a similar symptom but the replay did not mark this check as owned by it, so it is filed here; if they share a root cause, close them together.

## Evidence
Expected: CreateDistribution/GetDistribution publish a DomainName the emulator serves; after the distribution reaches Deployed, `GET https://<DomainName>/<marker-path>` returns 200 with the marker body fetched from the internal ALB through the VPC origin, and the same GET without the origin secret header returns the listener's fixed 403 body.

Actual: DomainName is `<distribution-id>.cloudfront.net`. The replay rejects it before any request is made: `simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:41385/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener` (the id and port are per-run values). Verdict: mismatch.

## Steps to Reproduce
1. Create a VPC with an internal ALB whose listener serves a marker only for requests carrying the X-Origin-Secret header and a fixed 403 otherwise.
2. Create a CloudFront VPC origin for that ALB and a distribution using it, with an origin custom header adding X-Origin-Secret.
3. Wait for the VPC origin and the distribution to reach Deployed.
4. Read Distribution.DomainName and issue `GET https://<DomainName>/<marker-path>`.
5. Observe the published DomainName is a cloudfront.net host on which the emulator listens for nothing, so the request cannot be made.
