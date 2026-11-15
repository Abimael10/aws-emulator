Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__elbv2.xsvc-elbv2-3-alb-security-group-ingress-revoked-client-connections-blocked-by-timeout ec2__elbv2.xsvc-elbv2-3-target-security-group-ingress-revoked-health-times-out-until-reauthorized
Issue-Key: ec2__elbv2.xsvc-elbv2-3-alb-security-group-ingress-revoked-client-connections-blocked-by-timeout|_alb_ips#24|EgressRefused|return sorted({a[4][0] for a in socket.getaddrinfo(dns, 80, socket.AF_INET, socket.SOCK_STREAM)})
Issue-Key: ec2__elbv2.xsvc-elbv2-3-target-security-group-ingress-revoked-health-times-out-until-reauthorized|_poll_until#23|Timeout|time.sleep(interval_s)
Issue-Commit: 2bc939698b9a959721d42e691c2ca676c39ad8ba

# ec2__elbv2: REOPENS — the ALB still announces an address that is not the served endpoint, and a revoked target security-group ingress still never changes target health

> _Note: this issue was identified against the code base at commit `2bc939698b9a959721d42e691c2ca676c39ad8ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeLoadBalancers

## Description
REOPENS 20261111T030957Z-ec2-elbv2-2-net-new-causes-the-alb.md: its fix left the call refused. That issue carries both of the check keys below in its Issue-Trace-Ids and is recorded as fixed, but both checks still fail on this replay — the first on an egress refusal of the address the emulator announces, the second without ever reaching a verdict. This is the same defect, not a new one.

=== Cause 1 of 2: the address the emulator announces for a load balancer is still not the endpoint it is reached on, so a client connection to it is refused ===
AWS: CreateLoadBalancer/DescribeLoadBalancers return LoadBalancer.DNSName, and a client that takes that name from the response and connects to it reaches the load balancer. The announced address is always an address the service actually serves.
Emulator: the host resolved from what the emulator returned is a different address from the endpoint under test and the connection is refused before any request is sent: `simparity.egress.EgressRefused: egress refused: '127.0.0.1:<port>' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on.` The announced value carries a port distinct from the emulator's own, so the data plane is unreachable. The prior issue's resolution recorded this cause as BLOCKED on a vendor listener API while marking the issue fixed; the refusal is unchanged.
Why: a client following the documented contract — read DNSName, connect to it — cannot reach the load balancer at all, so no data-plane behaviour can be observed. In particular, that revoking the ALB's own security-group ingress makes client connections time out and that re-authorizing restores them is untestable, and security-group enforcement at the load balancer cannot be relied on.
Fix: the load-balancer address publication in the elbv2 service — DNSName, and any URL derived from it, must name the host:port the emulator serves, and the data plane must accept connections there.
Traces: ec2__elbv2.xsvc-elbv2-3-alb-security-group-ingress-revoked-client-connections-blocked-by-timeout.

=== Cause 2 of 2: revoking the target instance's security-group ingress still never makes the target unhealthy ===
AWS: a target group health-checks its registered targets from the load balancer. Revoking the ingress rule that admits the load balancer's security group on the target's health-check port makes the probe time out, and after UnhealthyThresholdCount consecutive failures DescribeTargetHealth reports State='unhealthy' with Reason='Target.Timeout' (Smithy: elbv2 TargetHealthReasonEnum includes 'Target.Timeout'); re-authorizing the ingress returns the target to 'healthy' after HealthyThresholdCount successes.
Emulator: the check still never reaches a verdict and is killed with `Check timed out after 60.0s`; no transition out of the initial state is produced, consistent with the health probe not being subject to the EC2 security-group rules of the target and of the load balancer. The prior issue's resolution claimed this cause fixed by reading a registration's groups off the interfaces of the resource the registration names; on this replay the transition is still not observable.
Why: security groups are the primary cause of real 'target unhealthy' incidents. A client cannot reproduce or test that failure mode, and emulator-based validation of the security-group wiring between an ALB and its targets passes without exercising anything.
Fix: the target-group health-check scheduler in the elbv2 service must probe from the load balancer's network identity and that probe must be subject to the EC2 security-group rules of both the target and the load balancer, with the verdict settling — in both directions — inside a client's polling budget.
Traces: ec2__elbv2.xsvc-elbv2-3-target-security-group-ingress-revoked-health-times-out-until-reauthorized.

## Evidence
Cause 1 — ec2__elbv2.xsvc-elbv2-3-alb-security-group-ingress-revoked-client-connections-blocked-by-timeout: `Error: File "egress.py", line 338, in _hook / refuse(host, endpoint_host) / ... simparity.egress.EgressRefused: egress refused: '127.0.0.1:46251' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on.` The port 46251 is a per-run value; the requirement is that the announced address equal the served endpoint.

Cause 2 — ec2__elbv2.xsvc-elbv2-3-target-security-group-ingress-revoked-health-times-out-until-reauthorized: `Error: Check timed out after 60.0s`, no assertion reached. Expected: after RevokeSecurityGroupIngress on the target's health-check port, DescribeTargetHealth settles on State='unhealthy' / Reason='Target.Timeout', and after AuthorizeSecurityGroupIngress it returns to 'healthy'.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite so the emulator is serving.
2. Cause 1: create an internet-facing ALB with a security group, an HTTP:80 listener and a registered target; read DNSName from DescribeLoadBalancers and connect to it — the announced host:port is not the emulator's endpoint and the connection is refused, so the revoke/re-authorize client-connection behaviour cannot be exercised.
3. Cause 2: register an HTTP-serving instance into a target group behind an ALB listener, wait for 'healthy', RevokeSecurityGroupIngress for the ALB's security group on the target's health-check port and poll DescribeTargetHealth — no transition to 'unhealthy'/'Target.Timeout' occurs; re-authorize and observe no transition back either.
