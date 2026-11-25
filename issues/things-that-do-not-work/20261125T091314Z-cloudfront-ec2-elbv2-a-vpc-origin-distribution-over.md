Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge
Issue-Key: cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge|_poll_until#39|Timeout|time.sleep(interval_s)
Issue-Commit: 6651c87027da78b467298e3a28f696cadd44fc38

# cloudfront__ec2__elbv2: a VPC-origin distribution over an internal ALB never serves its marker at the edge, so the VPC-origin data-plane check reaches no verdict

> _Note: this issue was identified against the code base at commit `6651c87027da78b467298e3a28f696cadd44fc38`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateVpcOrigin

## Description
Cause 1 of 1: a distribution whose origin is a CloudFront VPC origin pointing at an internal Application Load Balancer never answers a viewer request, so the whole VPC-origin path is unobservable.
AWS: recorded behavior only (the viewer-facing edge is not modeled in Smithy). The recording shows the sequence CreateVpcOrigin -> poll GetVpcOrigin until the VPC origin is Deployed -> CreateDistribution with that VPC origin as its origin -> poll GetDistribution until Status "Deployed" -> HTTPS GET of the marker object through the distribution's published domain, answered 200 with the bytes the internal ALB's target serves. The internal ALB is reachable only from inside the VPC; the whole point of the VPC origin is that CloudFront, not the viewer, performs that private fetch.
Emulator: the check never reaches any of its assertions — it consumes its entire budget and the runner reports "Error: Check timed out after 60.0s". The scenario's own budgets (VPC-origin deploy 1200 s, distribution deploy 1200 s, ALB active 600 s, edge fetch 300 s) are never satisfied, so either the VPC origin / distribution never settles into a Deployed state or the published edge address serves nothing; in both cases no viewer response is produced and the marker is never seen.
Why: a client cannot use a VPC origin at all. It can create the resources but can never confirm that CloudFront fetches privately from the internal ALB, so the private-origin pattern (public edge in front of a non-internet-facing load balancer) cannot be exercised or validated against the emulator.
Fix: the CloudFront VPC-origin lifecycle (the state transition that must take a VPC origin and its distribution to Deployed within a bounded time) together with the CloudFront edge/viewer data plane that must accept a viewer request at the distribution's published address and fetch from the VPC origin's ALB target. This is the cloudfront__ec2__elbv2 facet of the same edge gap reported for plain cloudfront; it is tracked separately because the service heading differs and the VPC-origin leg is distinct.
Traces: cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge.

## Evidence
expected: after the VPC origin and the distribution both report Deployed, HTTPS GET of the marker key through the distribution's published DomainName returns 200 with the marker bytes served by the internal ALB's target.
actual:   Error: Check timed out after 60.0s — the check produces no response and no verdict; none of its waits (VPC-origin deploy, ALB active, distribution deploy, edge fetch) is ever satisfied.
The ALB DNS name, VPC/subnet ids, distribution id and marker body are per-run values; the requirement is that a Deployed VPC-origin distribution answers a viewer GET with the origin's bytes.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (run_behavioral_suite).
2. Replay cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge: create a VPC and an internal (scheme=internal) ALB with a target that serves a known marker body; CreateVpcOrigin for that ALB in us-east-1 and poll GetVpcOrigin until it is Deployed; CreateDistribution using that VPC origin and poll GetDistribution until Status == "Deployed".
3. HTTPS GET the marker key through the distribution's published DomainName. Expect 200 with the marker bytes; the emulator never answers and the check exhausts its budget.
