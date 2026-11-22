Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__elbv2.ecsnt-internet-facing-alb-in-a-vpc-without-an-internet-gateway-rejected
Issue-Key: ec2__elbv2.ecsnt-internet-facing-alb-in-a-vpc-without-an-internet-gateway-rejected|run#14|AssertionError|assert lb["DNSName"].lower().startswith("internal-"), f"internal DNS name: {lb['DNSName']!r}"
Issue-Commit: 8896a828822ada8fe26405eeb13d1bd63d2626fa

# ec2__elbv2: CreateLoadBalancer/DescribeLoadBalancers publish DNSName as a bare "host:port" address, so an internal-scheme load balancer's DNS name has none of AWS's form (no 'internal-' prefix)

> _Note: this issue was identified against the code base at commit `8896a828822ada8fe26405eeb13d1bd63d2626fa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateLoadBalancer

## Description
Cause 1 of 1: LoadBalancer.DNSName is published as a bare "<host>:<port>" address rather than a load-balancer DNS name, so the scheme-dependent form AWS guarantees — in particular the 'internal-' prefix that identifies an internal-scheme load balancer — is absent. Failing check: ec2__elbv2.ecsnt-internet-facing-alb-in-a-vpc-without-an-internet-gateway-rejected.

AWS: Smithy: elbv2 LoadBalancer.DNSName — "The public DNS name of the load balancer." AWS assigns that name from the load balancer's name, a generated suffix, the region and the Scheme requested at CreateLoadBalancer: an internal-scheme load balancer's DNS name begins with the fixed literal `internal-` and has the shape `internal-<name>-<generated-id>.<region>.elb.amazonaws.com`, while an internet-facing one has the same shape without that prefix. The fixed text in that template is `internal-`, `.elb.amazonaws.com` and the dot separators; the variable parts are the load balancer's own Name, the generated id and the region — all taken from the request and the service's state, never from one recording. DescribeLoadBalancers reports the same value, so the prefix is how a client tells an internal load balancer's address apart from a public one.

Emulator: DNSName is returned as a host-and-port pair with no name component and no scheme-derived prefix, e.g. `'127.0.0.1:44325'` for a load balancer created with Scheme='internal'. The host and port are per-run values; the divergence is that the value is an address literal rather than the scheme-dependent DNS name, so `DNSName.lower().startswith("internal-")` is false for an internal load balancer and the name encodes neither the requested Name nor the region.

Why: a client that distinguishes internal from internet-facing load balancers by their published DNS name — the standard way to assert that a load balancer was not exposed publicly, and the value that is handed to CloudFront origins, Route 53 alias targets and application configuration — sees no prefix and cannot make that distinction at all. The same value is also not a resolvable host name, so anything that treats DNSName as a DNS name (a CNAME target, a Host header, a certificate SAN check) is wrong.

Fix: the load-balancer address publication in the elbv2 service — the DNSName set at CreateLoadBalancer and reported by DescribeLoadBalancers must carry AWS's scheme-dependent DNS-name form (the 'internal-' prefix for Scheme='internal', the load balancer's Name, a generated id and the region), and the emulator's data plane must be reachable at the address that name denotes.

Related but not covering this: open issue 20261115T034057Z-ec2-elbv2-reopens-the-alb-still-announces-an.md (same service) reports that the announced load-balancer address is not the endpoint the emulator is reached on — an egress refusal — and 20261112T140934Z-elbv2-7-net-new-causes-loadbalancer-dnsname-is.md reports the "<host>:<port>" publication under the `elbv2` service. Neither carries this check, and neither states the scheme-dependent naming requirement that this check asserts.

Traces: ec2__elbv2.ecsnt-internet-facing-alb-in-a-vpc-without-an-internet-gateway-rejected.

## Evidence
Failing check ec2__elbv2.ecsnt-internet-facing-alb-in-a-vpc-without-an-internet-gateway-rejected [mismatch]. The earlier beats of the check pass — an internet-facing ALB in a VPC with no internet gateway is refused InvalidSubnet "VPC <vpc-id> has no internet gateway" and creates nothing — and the check then creates an internal-scheme ALB in the same subnets and reads it back:

  File "check_ec2__elbv2_ecsnt_internet_facing_alb_in_a_vpc_without_an_internet_gateway_rejected.py", line 43, in run
  assert lb["DNSName"].lower().startswith("internal-"), f"internal DNS name: {lb['DNSName']!r}"
  AssertionError: internal DNS name: '127.0.0.1:44325'

Expected: for Scheme='internal', DNSName is `internal-<name>-<generated-id>.<region>.elb.amazonaws.com` — the literal prefix `internal-` followed by the Name from the CreateLoadBalancer request, a generated id, the region and `.elb.amazonaws.com`. Actual: `'127.0.0.1:44325'`, an address literal with no prefix, no name and no region. The particular host/port shown is a per-run value.

## Steps to Reproduce
1. git_clone, then run the behavioral suite so the emulator is serving.
2. Re-replay ec2__elbv2.ecsnt-internet-facing-alb-in-a-vpc-without-an-internet-gateway-rejected.
3. Manually: create a VPC with no internet gateway, two subnets in different AZs and an HTTP security group.
4. CreateLoadBalancer with Type='application', Scheme='internet-facing' on those subnets — it is correctly refused InvalidSubnet.
5. CreateLoadBalancer with Type='application', Scheme='internal', the same Subnets and SecurityGroups; read DNSName from the response and from DescribeLoadBalancers. It must start with the literal 'internal-' and end in '.<region>.elb.amazonaws.com'; the emulator returns a '<host>:<port>' address instead.
