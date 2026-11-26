Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge
Issue-Key: cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge|_resolve#43|EgressRefused|return socket.gethostbyname_ex(host)
Issue-Commit: 9f7bf23473ff024f6e39d27cef35bcce93712a4b

# cloudfront__ec2__elbv2: a VPC-origin distribution announces its edge on 127.0.0.1:<random port>, an address the emulator is not reached on, so the edge check is refused at connect

> _Note: this issue was identified against the code base at commit `9f7bf23473ff024f6e39d27cef35bcce93712a4b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: the DomainName a Deployed CloudFront distribution publishes for a VPC origin over an internal ALB is an address the emulator does not serve. The viewer request is refused before any HTTP exchange happens, so nothing about the VPC-origin data plane can be observed.

AWS: recorded behavior only for the data plane (CloudFront's Smithy model types Distribution.DomainName as a plain String with no constraint, so the model states nothing about its form). What AWS guarantees is behavioral and is what the recording exercises: once GetDistribution reports Status `Deployed`, the host in DomainName is resolvable and answers viewer HTTPS/HTTP on the standard port, and a GET carrying the configured origin-secret header is served by the VPC origin's internal ALB (while a GET without it gets the listener's fixed 403 with body `sim2real-request-without-origin-secret-denied`). The published DomainName must therefore be an address the serving endpoint actually accepts connections on.

Emulator: the distribution deploys, but the address derived from the published DomainName is `127.0.0.1:39861` — a loopback host on a port that is not the endpoint under test. The run's egress guard refuses it: `simparity.egress.EgressRefused: egress refused: '127.0.0.1:39861' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on`, surfaced to the check as `CapabilityNotOffered: the target serves no address for 127.0.0.1:39861`. The host and port are per-run values; the defect is that the announced authority is not the authority the emulator is reached on.

Why: a client that follows CloudFront's own contract — deploy, read DomainName, request it — cannot reach the edge at all. Every VPC-origin behavior that only shows at the edge (origin-secret header enforcement, the listener's fixed-403 path, cache behavior, the Via header) is unobservable, and a caller gets a connection refusal where AWS serves a response.

Fix: the CloudFront distribution serializer / endpoint-advertising layer that fills Distribution.DomainName (and the same value in GetDistribution / ListDistributions readback) must publish an authority the emulator actually listens on for viewer traffic, and the edge listener must accept viewer requests on it. Related open issues describe other DomainName forms for this same scenario but do not cover this one: 20261125T091314Z-cloudfront-ec2-elbv2-a-vpc-origin-distribution-over.md (the marker is never served) and 20261125T150300Z-cloudfront-ec2-elbv2-a-vpc-origin-distribution-publishes.md (DomainName published as an unserved <id>.cloudfront.net host). This check now fails on a DIFFERENT announced authority — a loopback host with a non-endpoint port — so it is a distinct cause from the cloudfront.net-host cluster.

Traces: cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge.

## Evidence
Check: cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge

Expected (AWS): after GetDistribution reports Status=Deployed, a viewer GET to the published DomainName reaches the edge; with the X-Origin-Secret header the internal ALB's marker body is served, and without it the listener's fixed 403 `sim2real-request-without-origin-secret-denied` is served.

Actual (emulator):
  Error: CapabilityNotOffered: the target serves no address for 127.0.0.1:39861
    File "egress.py", line 338, in _hook -> refuse(host, endpoint_host)
    File "egress.py", line 291, in refuse -> raise EgressRefused(...)
  simparity.egress.EgressRefused: egress refused: '127.0.0.1:39861' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on

No HTTP status, error code or body is produced at all — the connection is refused before the request is sent. `127.0.0.1` and `39861` are per-run values; the requirement is that the announced authority equals the authority the emulator serves viewer traffic on.

## Steps to Reproduce
1. Create a VPC, a private subnet and a security group; create an internal (scheme=internal) Application Load Balancer with an HTTP listener whose default action is a fixed 403 response with body `sim2real-request-without-origin-secret-denied`, plus a rule that requires the X-Origin-Secret header and forwards to a target group serving a marker body. Wait for the ALB to become active.
2. CreateVpcOrigin for that ALB and wait for it to deploy.
3. CreateDistribution (signed in us-east-1) with that VPC origin, configured to add the X-Origin-Secret header to origin requests. Poll GetDistribution until Status=Deployed.
4. Read Distribution.DomainName from the response and issue a plain viewer GET to it.
5. The connection is refused: the announced authority is 127.0.0.1:<random high port>, not the authority the emulator is reached on. AWS serves the marker body here.
