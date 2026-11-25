Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elb.cross-zone-load-balancing-spreads-requests-beyond-the-node-zone
Issue-Shape-Id: com.amazonaws.elasticloadbalancing#ElasticLoadBalancing_v7
Issue-Key: elb.cross-zone-load-balancing-spreads-requests-beyond-the-node-zone|_nodes#19|EgressRefused|infos = socket.getaddrinfo(lb["dns"], 80, socket.AF_INET, socket.SOCK_STREAM)
Issue-Commit: 2bac9c0c7897d6c2f9107b98647560d403e99572

# REOPENS: ELB Classic still publishes a DNSName the emulator is not reached on (a loopback host:port), so every Classic data-plane request is refused

> _Note: this issue was identified against the code base at commit `2bac9c0c7897d6c2f9107b98647560d403e99572`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateLoadBalancer

## Description
REOPENS 20261110T183056Z-elb-classic-2-net-new-causes-the-published.md: its fix left the call refused.

That issue's Cause 1 required CreateLoadBalancer to publish a DNSName the emulator itself answers on. The fix replaced the *.elb.amazonaws.com host with a per-load-balancer loopback socket of the emulator's own choosing and publishes that socket's "host:port" as DNSName. The address is still not the endpoint the emulator is reached on, so the first data-plane GET of the published name is still refused before it leaves the runner and no leg of the cross-zone scenario is ever evaluated.

Cause 1 of 1 — the published DNSName is an address other than the endpoint under test. Failing check: elb.cross-zone-load-balancing-spreads-requests-beyond-the-node-zone.
AWS: Smithy (com.amazonaws.elasticloadbalancing#ElasticLoadBalancing_v7): CreateLoadBalancer input CreateAccessPointInput {LoadBalancerName required, Listeners required, AvailabilityZones/Subnets/SecurityGroups/Scheme/Tags}, output CreateAccessPointOutput {DNSName: DNSName}; DescribeLoadBalancers reports the same DNSName on LoadBalancerDescription. The contract of DNSName is that it is the address a client sends real traffic to: a client resolves that NAME and connects to the listener port (80 here), the load balancer accepts the connection and forwards to its registered InService instances, spreading across every enabled zone once ModifyLoadBalancerAttributes sets CrossZoneLoadBalancing.Enabled=true. The generated name itself is a per-run value and is not a requirement; what IS required is that the address published is one the emulator answers on at the endpoint it is reached on, because an emulator run may reach only the emulator.
Emulator: DNSName is published as a bare authority of the form "<loopback-host>:<port>" naming a second socket the service opens per load balancer (the replay observed '127.0.0.1:35253'; the host and the port digits are per-run values, publishing a second address rather than the endpoint's own is the defect). The check resolves the published name and connects to port 80, and the call is refused: simparity.egress.EgressRefused: egress refused: '127.0.0.1:35253' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on. No cross-zone assertion runs.
Why: a client that uses DNSName as AWS documents it — an HTTP client, a health probe, a Route 53 alias target, an integration test — is pointed at an address that is neither a resolvable host nor the endpoint it reached the API on. The published value is also not a hostname at all (it carries an embedded port), so any client that resolves it, or that places it in a Host header or a DNS record, fails on the string itself. The whole Classic data plane (listener forwarding, cross-zone spreading, stickiness) stays unreachable through the only address AWS gives a client.
Fix: the published-address construction in the CreateLoadBalancer handler and the identical projection in DescribeLoadBalancers / the load balancer's state, together with the socket that address comes from — the published DNSName must be a host reachable AT the endpoint the emulator is reached on (a name routed by Host header on the endpoint's own port, which is the addressing the suite's endpoint_route helper assumes), not an extra loopback port of the emulator's choosing, and not a value with a port embedded in it.
Traces: check elb.cross-zone-load-balancing-spreads-requests-beyond-the-node-zone. Code: services/service-com-amazonaws-elasticloadbalancing-elasticloadbalancing-v7/src/state/load_balancer_endpoint.rs, src/service/listener_socket.rs, src/service/data_plane.rs, src/ops/create_load_balancer.rs, src/ops/describe_load_balancers.rs. The same announced-address defect is open for ELBv2 and does NOT cover this check: 20261111T030957Z-ec2-elbv2-2-net-new-causes-the-alb.md and 20261112T140934Z-elbv2-7-net-new-causes-loadbalancer-dnsname-is.md.

## Evidence
Check elb.cross-zone-load-balancing-spreads-requests-beyond-the-node-zone, verdict [error].
Expected (AWS): CreateLoadBalancer returns a DNSName that resolves; GET http://<DNSName>/ is answered by the load balancer, and with CrossZoneLoadBalancing.Enabled=true a single node returns HTTP 200 bodies from registered InService backends in BOTH enabled availability zones.
Actual (emulator), verbatim from the replay:
  File "egress.py", line 338, in _hook / refuse(host, endpoint_host)
  File "egress.py", line 291, in refuse / raise EgressRefused(
  simparity.egress.EgressRefused: egress refused: '127.0.0.1:35253' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on
The loopback address and the port 35253 are per-run values; the requirement they violate is that the published DNSName must be the address the emulator is reached on, expressed as a host a client can resolve.
Model evidence: CreateLoadBalancer -> CreateAccessPointOutput {DNSName}; DescribeLoadBalancers reports the same DNSName, so DNSName is the only address a Classic client is given.
Prior fix: 20261110T183056Z-elb-classic-2-net-new-causes-the-published.md (status fixed) records this publication change in src/state/load_balancer_endpoint.rs and itself notes the residual gap; the check is still refused.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the behavioral suite (simparity-behavioral-aws-v1), elb checks.
2. Replay elb.cross-zone-load-balancing-spreads-requests-beyond-the-node-zone: it builds a VPC with two subnets in two usable AZs, launches one HTTP backend per subnet, calls CreateLoadBalancer with both subnets, ConfigureHealthCheck('HTTP:80/ping'), ModifyLoadBalancerAttributes with CrossZoneLoadBalancing.Enabled=true and RegisterInstancesWithLoadBalancer for both instances, then resolves the published DNSName and GETs port 80 on it.
3. Observe the DNSName returned by CreateLoadBalancer is a '<host>:<port>' authority naming a port other than the endpoint the API call was made to, and the first data-plane request is refused with simparity.egress.EgressRefused ('... is not the endpoint under test').
4. Expected after a fix: DescribeLoadBalancers/CreateLoadBalancer publish a resolvable host served at the endpoint under test; GET http://<DNSName>/ returns 200 and, with cross-zone enabled, successive GETs are answered from backends in both AZs.
