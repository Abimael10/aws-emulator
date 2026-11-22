Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__ec2__elbv2.vpc-origin-duplicate-name-rejected cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge
Issue-Key: cloudfront__ec2__elbv2.vpc-origin-duplicate-name-rejected|run#28|AssertionError|raise AssertionError(f"a second VPC origin reusing the name {name!r} was accepted")
Issue-Key: cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge|_resolve#43|EgressRefused|return socket.gethostbyname_ex(host)
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# cloudfront__ec2__elbv2: 2 net-new causes — CreateVpcOrigin accepts a duplicate Name, and the internal ALB announces an elb.amazonaws.com host it does not serve

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateVpcOrigin

## Description
Two net-new root causes in the CloudFront VPC-origin surface over an ELBv2 load balancer.

Cause 1 of 2 — CreateVpcOrigin does not enforce Name uniqueness.
AWS: recorded behavior only for the exact wording (the CloudFront model declares `EntityAlreadyExists` for CreateVpcOrigin but documents the uniqueness rule in prose): a second VPC origin whose VpcOriginEndpointConfig.Name equals an existing one's is refused with error code `EntityAlreadyExists` and message `Another vpc origin with the same name already exists within the aws account.` — that message is entirely fixed text, with no value interpolated from the request — and nothing is created. The identical configuration under a different Name succeeds and GetVpcOrigin reads it back unchanged.
Emulator: the duplicate-name CreateVpcOrigin SUCCEEDS; a second VPC origin is created under the same Name, so ListVpcOrigins then shows that name twice.
Why: a client that relies on the name being an account-unique handle (looking an origin up by name, or retrying a create idempotently) silently ends up with two origins sharing one name and resolves the wrong one; the negative control that should fail passes.
Fix: CreateVpcOrigin validation/state in the CloudFront handler — reject a Name already held by an existing VPC origin in the account with the modeled EntityAlreadyExists error, creating nothing.
Traces: cloudfront__ec2__elbv2.vpc-origin-duplicate-name-rejected.

Cause 2 of 2 — the internal ALB behind the VPC origin announces an address that is not the endpoint under test.
AWS: Smithy — `LoadBalancer.DNSName`; AWS publishes the name a client can actually reach the load balancer on, and a CloudFront distribution over a VPC origin serves the origin's bytes at the distribution's domain. Recorded behavior: the check resolves and requests the load balancer / distribution to read back the origin's marker body and the listener's fixed 403 for a request missing X-Origin-Secret.
Emulator: the emulator announces `internal-<name>-<digits>.us-west-2.elb.amazonaws.com` — a real-AWS host it does not serve — so the request is refused before it can be made: `EgressRefused: '…elb.amazonaws.com' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator`, reported as `CapabilityNotOffered: the target serves no address for …`. The announced address is the emulator's own output, so this is the emulator announcing an address other than the one it is reached on.
Why: every client that takes DNSName (directly, or via a CloudFront VPC origin / distribution) and sends traffic to it reaches nothing. The whole data plane behind a VPC origin is unreachable, and the failure surfaces as a connection/name failure rather than an AWS error.
Fix: the ELBv2 load balancer address publication — DNSName must be the host:port the emulator actually serves, and the CloudFront VPC-origin/distribution data path must forward to that served address.
Traces: cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge.
Related but not covering: 20261112T140934Z-elbv2-7-net-new-causes-loadbalancer-dnsname-is.md and 20261115T034057Z-ec2-elbv2-reopens-the-alb-still-announces-an.md record the same announcement defect under the elbv2 and ec2__elbv2 tokens; this one is the CloudFront VPC-origin path.

## Evidence
Cause 1 — expected: the second CreateVpcOrigin reusing an existing Name raises ClientError ('EntityAlreadyExists', 'Another vpc origin with the same name already exists within the aws account.') and creates nothing.
Actual (replay):
  AssertionError: a second VPC origin reusing the name 'sim2real-vod-<suffix>' was accepted
  (check_cloudfront__ec2__elbv2_vpc_origin_duplicate_name_rejected.py line 234)
(the name is a per-run generated value; the requirement is the refusal, not that string)

Cause 2 — expected: a request to the distribution/load balancer returns the origin's marker body with the secret header, and the listener's fixed 403 body 'sim2real-request-without-origin-secret-denied' without it.
Actual (replay):
  CapabilityNotOffered: the target serves no address for internal-sim2real-vo-<suffix>.us-west-2.elb.amazonaws.com
  simparity.egress.EgressRefused: egress refused: '…us-west-2.elb.amazonaws.com' is not the endpoint under test (127.0.0.1)
(the host label and the served endpoint's port are per-run; the requirement is that DNSName names the served endpoint)

## Steps to Reproduce
Cause 1:
1. Create a VPC with two subnets, a security group, and an internal ALB; wait for State.Code=active.
2. cloudfront.create_vpc_origin(VpcOriginEndpointConfig={Name: N, Arn: <alb arn>, HTTPPort: 80, HTTPSPort: 443, OriginProtocolPolicy: 'http-only'}) — succeeds.
3. Call it again with the SAME Name N and the same config.
Observed: a second origin is created. Expected: EntityAlreadyExists / 'Another vpc origin with the same name already exists within the aws account.'
4. Repeat with a different Name: it must succeed and GetVpcOrigin must read it back unchanged; ListVpcOrigins must show each name exactly once.

Cause 2:
1. Create an internal ALB with a listener whose default action is a fixed 403 body and a rule requiring header X-Origin-Secret that forwards to a target serving a marker body.
2. Create a CloudFront VPC origin over that ALB and a distribution using it; wait for Deployed.
3. GET the distribution domain with and without X-Origin-Secret.
Observed: the request cannot be sent — the announced elb.amazonaws.com host is not the served endpoint. Expected: the marker body with the header, the fixed 403 body without it.

## Resolution

Cause 1 was already closed before this change: `CreateVpcOrigin` refuses a
`VpcOriginEndpointConfig$Name` the account already holds a VPC origin under with the modelled
`EntityAlreadyExists` and its fixed sentence, minting nothing
(`services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/ops/create_vpc_origin.rs`
over `state::holds_vpc_origin_named` / `state::vpc_origin_already_exists`), pinned by
`tests/all/vpc_origin_create_refusals.rs`. The replay reports that cause fixed.

Cause 2 was implemented in both halves this change owns.

* The address publication: `CreateLoadBalancer` now opens a core-owned loopback listener for
  every load balancer whose listeners carry a client's own request — the Application and the
  Network family — under EITHER scheme, and publishes that socket's `host:port` as `DNSName`
  (`services/service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10/src/ops/create_load_balancer.rs`,
  `src/state/load_balancer_endpoint.rs`). `Scheme` decides which clients may reach a load
  balancer in a real network, not whether the published value is the address it answers on; a
  Gateway Load Balancer keeps the host Elastic Load Balancing's own zone gives it, its `GENEVE`
  listeners carrying no request of that kind.
* The VPC-origin data path: the edge now reads the protocol policy and the ports of a
  VPC-origin fetch off the `VpcOriginEndpointConfig` the VPC origin resource holds, instead of
  addressing such an origin as an Amazon S3 one
  (`services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/service/edge.rs`), so a
  distribution over a VPC origin serves what the load balancer answers the origin request —
  its rule's body for the request carrying the origin's `CustomHeaders`, its default action's
  for the one that does not. Pinned end to end by
  `core/router/tests/all/cloudfront_vpc_origin_over_an_internal_load_balancer.rs`.

Residual gap, recorded rather than left implicit: the recorded check
`cloudfront__ec2__elbv2.vpc-origin-internal-alb-serves-marker-at-the-edge` still does not pass.
It hands the published value to a DNS resolver (`socket.gethostbyname_ex(host)`), so it needs a
BARE host that resolves plus a server on the scheme's default port. The core listener API vends
a port of its own choosing and takes no requested port (`vendor/server/src/listeners.rs`,
`serve_native` binding `bind_ip_from_env()` within `listener_port_range_from_env()`), and a bare
loopback host would announce an address nothing answers — and would make one host the `DNSName`
of every load balancer, so the data plane could resolve none of them. The same residual is
recorded in `issues/things-that-now-work/20261110T183056Z-elb-classic-2-net-new-causes-the-published.md`
and as cause C of
`issues/things-that-do-not-work/20261118T090000Z-elbv2-target-health-on-silence-alpn-poll-ssl-policy-and-dnsname-port-addressing-remain-open.md`,
which owns the core change it needs.
