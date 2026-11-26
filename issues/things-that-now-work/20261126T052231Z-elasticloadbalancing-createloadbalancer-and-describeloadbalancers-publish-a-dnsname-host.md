Issue-Source: trace-sweep-divergence
Issue-Trace: elbclassic-create-load-balancer-validation-and-duplicate-name
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elbclassic-create-load-balancer-validation-and-duplicate-name elbclassic-health-check-configuration-round-trip-and-validation elbclassic-listener-lifecycle-duplicate-and-idempotent-delete elbclassic-load-balancer-lifecycle-create-describe-delete elbclassic-policy-lifecycle-cookie-stickiness-and-generic-policy elbclassic-subnet-attach-detach-and-security-group-reassignment
Issue-Shape-Id: com.amazonaws.elasticloadbalancing#ElasticLoadBalancing_v7
Issue-Key: elasticloadbalancing|DescribeLoadBalancers|2xx->2xx|$.DescribeLoadBalancersResponse.DescribeLoadBalancersResult.LoadBalancerDescriptions.member.DNSName
Issue-Key: elasticloadbalancing|CreateLoadBalancer|2xx->2xx|$.CreateLoadBalancerResponse.CreateLoadBalancerResult.DNSName
Issue-Commit: 384f5f940fcbeb20a0d7a3c3cf2d9778f9da2b04

# elasticloadbalancing: CreateLoadBalancer and DescribeLoadBalancers publish a DNSName host the emulator is not reached on

> _Note: this issue was identified against the code base at commit `384f5f940fcbeb20a0d7a3c3cf2d9778f9da2b04`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateLoadBalancer

## Description
Two net-new causes, one per operation that publishes the Classic load balancer's address. Both are the same fault seen on two wire paths: the value published as DNSName is a host in the loopback DNS zone that the emulator does not answer on, so the only address a Classic client is ever handed is unreachable. The recorded responses do not assert the literal AWS hostname (the replay marks the expected value '<shaped-not-asserted>'), so the generated name, the id suffix, the region label and the port are per-run values and are NOT the requirement; the requirement is that the published address is one the service answers on for the client that was handed it, with no scheme and no embedded port.

Cause 1 of 2: DescribeLoadBalancers reports an unreachable DNSName (fixture elbclassic-create-load-balancer-validation-and-duplicate-name, step task#5; 8 further recorded steps in the same cluster).
AWS: Smithy (com.amazonaws.elasticloadbalancing#ElasticLoadBalancing_v7): DescribeLoadBalancers -> DescribeAccessPointsOutput {LoadBalancerDescriptions: list<LoadBalancerDescription>, NextMarker}, each LoadBalancerDescription carrying DNSName (shape DNSName, string); errors AccessPointNotFoundException (awsQueryError LoadBalancerNotFound, 400) and DependencyThrottleException (DependencyThrottle, 400). DNSName's contract is operational and is recorded behavior for the host itself: AWS publishes a bare resolvable host of the form [internal-]<name>-<id>.<region>.elb.amazonaws.com (the 'internal-' prefix is fixed text carried only by the internal scheme; name, id and region are values), a client resolves it and connects on the listener's LoadBalancerPort, and the load balancer answers there. The status stays 200 and the body shape is otherwise correct in the recording.
Emulator: the response is a well-formed 200 whose $.DescribeLoadBalancersResponse.DescribeLoadBalancersResult.LoadBalancerDescriptions.member.DNSName is a name in the loopback zone — the replay reports verdict host_not_own for a value of the form 'internal-<name>-<id>.<region>.elb.localhost' — i.e. a host that is not one of the emulator's own hosts and that nothing answers on, while the API itself was served on the emulator's own endpoint authority.
Why: a client that uses DNSName exactly as AWS documents it (an HTTP client, an external health probe, a Route 53 alias target, an integration test) is pointed at a host nothing resolves to the serving interface and nothing listens on, so no Classic data-plane behavior is reachable through the only address AWS gives a caller, even though the control-plane calls all succeed.
Fix: the published-address projection shared by the DescribeLoadBalancers handler and the stored load balancer endpoint, plus the data-plane routing that must claim the published host, so the name published is an address the receiving client can reach (the emulator's own served authority, or a per-resource listener's authority whose Host route the load balancer plane serves).
Traces: elbclassic-create-load-balancer-validation-and-duplicate-name#task#5 (representative), plus elbclassic-health-check-configuration-round-trip-and-validation#task#6 and #task#10, elbclassic-listener-lifecycle-duplicate-and-idempotent-delete#task#6 and #task#10, elbclassic-load-balancer-lifecycle-create-describe-delete#task#5, elbclassic-policy-lifecycle-cookie-stickiness-and-generic-policy#task#12, elbclassic-subnet-attach-detach-and-security-group-reassignment#task#9 and #task#13. Code: services/service-com-amazonaws-elasticloadbalancing-elasticloadbalancing-v7/src/state/load_balancer_endpoint.rs (published_dns_name lines 109-123, announced_zone lines 138-146, endpoint_authority lines 148+), src/ops/describe_load_balancers.rs.

Cause 2 of 2: CreateLoadBalancer returns the same unreachable DNSName at create time (fixture elbclassic-load-balancer-lifecycle-create-describe-delete, step task#4).
AWS: Smithy: CreateLoadBalancer input CreateAccessPointInput {LoadBalancerName required, Listeners required, AvailabilityZones/Subnets/SecurityGroups/Scheme/Tags}, output CreateAccessPointOutput {DNSName}; modeled errors include DuplicateAccessPointNameException (DuplicateLoadBalancerName, 400), InvalidSchemeException (InvalidScheme, 400), InvalidSubnetException (InvalidSubnet, 400). The DNSName returned by the create is the same address the describe reports and carries the same operational contract: a bare resolvable host, no scheme, no embedded port, answered on the listener port.
Emulator: $.CreateLoadBalancerResponse.CreateLoadBalancerResult.DNSName is minted in the loopback zone — the replay reports verdict host_not_own for a value of the form 'internal-<name>-<id>.<region>.elb.localhost' — so the address is wrong from the moment the resource is created, not only when it is read back.
Why: the create response is the first and often only place a client reads the address from (it is what a deploy script, a CloudFormation-style output or a DNS alias is wired from), so the unreachable host propagates into every downstream consumer before any describe is issued.
Fix: the same published-address construction, at the point the CreateLoadBalancer handler mints and stores the name, so create and describe publish one address the emulator actually serves.
Traces: elbclassic-load-balancer-lifecycle-create-describe-delete#task#4. Code: services/service-com-amazonaws-elasticloadbalancing-elasticloadbalancing-v7/src/ops/create_load_balancer.rs, src/state/load_balancer_endpoint.rs (published_dns_name lines 109-123, open_load_balancer_endpoint).

Related open issues that do NOT cover these causes (the replay marks both clusters owned_by [], so no filed issue carries their replay keys; those issues are grounded in behavioral check ids, not in these fixture steps): 20261125T211644Z-elb-reopens-the-classic-load-balancer-s-published.md (same fault from the behavioral suite), 20261124T011423Z-elb-2-net-new-causes-a-registered-classic--r2.md (its Cause 2), and the ELBv2 twin 20261122T170634Z-ec2-elbv2-createloadbalancers-publish-dnsname-as-a.md.

## Evidence
Replay of simparity-wire-aws-v1 against this checkout: 166 steps, 154 passed, 12 mismatched, score 92.8%, emulator_up true (/__admin/health 200, data listener accepting).

Cause 1 — cluster representative elbclassic-create-load-balancer-validation-and-duplicate-name#task#5, operation DescribeLoadBalancers, verdict mismatch, expected_status 200 / actual_status 200, affected_count 9:
  detail: $.DescribeLoadBalancersResponse.DescribeLoadBalancersResult.LoadBalancerDescriptions.member.DNSName: host_not_own (expected '<shaped-not-asserted>', got 'internal-sim2real-probe-clb-8bd717b0-5258351275.us-west-2.elb.localhost')
  affected: elbclassic-create-load-balancer-validation-and-duplicate-name#task#5, elbclassic-health-check-configuration-round-trip-and-validation#task#6, elbclassic-health-check-configuration-round-trip-and-validation#task#10, elbclassic-listener-lifecycle-duplicate-and-idempotent-delete#task#6, elbclassic-listener-lifecycle-duplicate-and-idempotent-delete#task#10, elbclassic-load-balancer-lifecycle-create-describe-delete#task#5, elbclassic-policy-lifecycle-cookie-stickiness-and-generic-policy#task#12, elbclassic-subnet-attach-detach-and-security-group-reassignment#task#9, elbclassic-subnet-attach-detach-and-security-group-reassignment#task#13

Cause 2 — cluster representative elbclassic-load-balancer-lifecycle-create-describe-delete#task#4, operation CreateLoadBalancer, verdict mismatch, expected_status 200 / actual_status 200, affected_count 1:
  detail: $.CreateLoadBalancerResponse.CreateLoadBalancerResult.DNSName: host_not_own (expected '<shaped-not-asserted>', got 'internal-sim2real-probe-clb-a451cb52-9120689121.us-west-2.elb.localhost')

In both details the load balancer name ('sim2real-probe-clb'), the id suffixes (8bd717b0-5258351275 / a451cb52-9120689121) and the region label are per-run values and are not part of the requirement; the 'internal-' prefix is fixed text for the internal scheme. The invariant violated is identical in both: the host published as DNSName is not an address the emulator answers on ('.elb.localhost', the loopback zone), while the recorded AWS value is a resolvable [internal-]<name>-<id>.<region>.elb.amazonaws.com host served on the listener port.

Expected (AWS): CreateLoadBalancer returns, and DescribeLoadBalancers reports, one and the same bare host (no scheme, no embedded port) that resolves and accepts a connection on the listener's LoadBalancerPort.
Actual (emulator): both publish a '*.elb.localhost' host that is not one of the emulator's own hosts and that no listener serves.
Code: services/service-com-amazonaws-elasticloadbalancing-elasticloadbalancing-v7/src/state/load_balancer_endpoint.rs lines 95-146 construct the name as `{prefix}{label}-{id}.{region}.elb.{zone}` with zone = LOOPBACK_ZONE whenever the request reached no endpoint of the emulator's own.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Replay fixture elbclassic-load-balancer-lifecycle-create-describe-delete step task#4 (CreateLoadBalancer, Scheme=internal, one HTTP:80 listener) and read CreateLoadBalancerResult.DNSName.
3. Replay fixture elbclassic-create-load-balancer-validation-and-duplicate-name step task#5 (DescribeLoadBalancers) and read LoadBalancerDescriptions.member.DNSName; the same value is reported by the 8 other steps listed in the cluster.
4. Observe the published value is '[internal-]<name>-<id>.<region>.elb.localhost' — a host in the loopback zone that the emulator does not answer on — while the API call itself was served on the emulator's own authority (127.0.0.1:<port>).
5. Resolve that host and connect to it on the listener port (80): nothing answers; the replay grades the field host_not_own.
6. Expected after a fix: both responses publish one bare host, with no scheme and no port, that the client which received it can resolve and connect to on the listener port, and the load balancer's own data plane serves that Host route on the endpoint the caller already reaches.
