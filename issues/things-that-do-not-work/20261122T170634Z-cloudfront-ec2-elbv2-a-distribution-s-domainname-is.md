Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge
Issue-Key: cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge|_resolve#43|EgressRefused|return socket.gethostbyname_ex(host)
Issue-Commit: 8896a828822ada8fe26405eeb13d1bd63d2626fa

# cloudfront__ec2__elbv2: a distribution's DomainName is announced as a bare "host:port" address that is not the endpoint the emulator serves, so the VPC-origin edge path is unreachable

> _Note: this issue was identified against the code base at commit `8896a828822ada8fe26405eeb13d1bd63d2626fa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: the address CloudFront publishes for a distribution (Distribution.DomainName, read back by the check from the CreateDistribution/GetDistribution response) is a literal "<host>:<port>" string whose port is not the port the emulator is reached on, so a viewer request built from it never reaches the emulator at all. Failing check: cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge.

AWS: CreateDistribution returns Distribution.DomainName — the domain name assigned to the distribution, e.g. <distribution-id>.cloudfront.net — and a viewer that takes that value from the response and issues `GET https://<DomainName>/<path>` reaches the distribution's edge. The announced value is a host name (no port component) that resolves to an address the service actually serves. Recorded behavior only for the emulator's own addressing convention; what is a requirement is the contract that the announced DomainName is an address the serving endpoint answers on.

Emulator: the value announced for DomainName carries a port distinct from the endpoint under test, so the request is refused before it is ever sent: `CapabilityNotOffered: the target serves no address for 127.0.0.1:37157` / `simparity.egress.EgressRefused: egress refused: '127.0.0.1:37157' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on`. The host and port in that message are per-run values; the defect is that the announced address is not the served one.

Why: a client following the documented contract — create a distribution, read DomainName, request through it — cannot reach the edge at all. Nothing on the CloudFront data path can be exercised: in particular a VPC origin over an internal ALB, the origin custom header the ALB's listener rule requires, and the ALB's fixed-response body appearing at the edge are all unobservable, so emulator-based validation of a VPC-origin distribution passes without ever serving a byte.

Fix: the CloudFront distribution address publication and its edge data plane — the DomainName returned by CreateDistribution/GetDistribution/ListDistributions (and any URL derived from it) must name the host and port the emulator actually serves, and the edge must answer viewer requests there.

Related but not covering this: open issue 20261109T152222Z-apigateway-cloudfront-s3-a-distribution-s-domainname-is.md reports DomainName published as the real-AWS <id>.cloudfront.net host, and 20261115T163206Z-cloudfront-elbv2-a-cloudfront-distribution-over-an-alb.md reports an ALB-custom-origin distribution not serving at the edge. Neither carries this check or this announced-address-with-a-foreign-port failure.

Traces: cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge.

## Evidence
Failing check cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge [mismatch]. The check reaches `edge, route = ctx.endpoint_route(dist["DomainName"])` and then GETs `{edge}/sim2real/resource`:

  Error: CapabilityNotOffered: the target serves no address for 127.0.0.1:37157
  File "egress.py", line 338, in _hook / refuse(host, endpoint_host)
  simparity.egress.EgressRefused: egress refused: '127.0.0.1:37157' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on

Expected: the DomainName returned for the distribution is an address the serving endpoint answers on, and `GET /<path>` through it returns HTTP 200 with the ALB listener's fixed-response marker body. Actual: the announced value is a host:port whose port is not the served endpoint's, and no request is ever sent. The literal 127.0.0.1 and 37157 are this run's values, not a requirement.

## Steps to Reproduce
1. git_clone, then run the behavioral suite so the emulator is serving.
2. Re-replay cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge.
3. Manually: create a VPC with subnets and a security group, create an internal Application Load Balancer with an HTTP:80 listener whose rule requires a secret origin header and otherwise returns a fixed 403; wait for State.Code='active'.
4. CreateVpcOrigin on the ALB ARN, then CreateDistribution with one origin whose DomainName is the ALB's DNSName, a VpcOriginConfig naming that VPC origin, and a custom header carrying the secret.
5. Read Distribution.DomainName from the response and issue `GET https://<DomainName>/sim2real/resource`. The announced value is a "host:port" string pointing at a port the emulator does not serve, so the request cannot be delivered; on AWS it returns 200 with the ALB's fixed-response body.
