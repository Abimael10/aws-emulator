Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elbv2.e2e-alb-traffic-lifecycle-flow elbv2.elbv2attr-deep-elbv2-1-unhealthy-state-routing-min-healthy-count-serves-unhealthy-target elbv2.elbv2shape-deep-elbv2-3-app-cookie-stickiness-pins-with-cookie-spreads-without elbv2.elbv2shape-deep-elbv2-3-least-outstanding-requests-starves-slow-backend-vs-round-robin elbv2.elbv2shape-deep-elbv2-3-slow-start-with-least-outstanding-requests-rejected elbv2.nlb-tcp-listener-preserves-client-source-ip-to-instance-target elbv2.elbv2shape-deep-elbv2-3-oversize-request-line-and-headers-rejected-by-alb elbv2.deep-elbv2-2-nlbwire-proxy-protocol-v2-signature-and-address-block elbv2.deep-elbv2-2-nlbwire-security-group-unlisted-listener-port-blocked elbv2.deep-elbv2-2-nlbwire-cross-zone-target-group-override elbv2.lb-cookie-stickiness-pins-a-client-to-one-target-in-the-group elbv2.e2e-alb-target-routing-lifecycle
Issue-Shape-Id: com.amazonaws.elasticloadbalancingv2#ElasticLoadBalancing_v10
Issue-Key: elbv2.e2e-alb-traffic-lifecycle-flow|_poll_until#55|Timeout|time.sleep(interval_s)
Issue-Key: elbv2.elbv2attr-deep-elbv2-1-unhealthy-state-routing-min-healthy-count-serves-unhealthy-target|_poll_until#23|Timeout|last = probe()
Issue-Key: elbv2.elbv2shape-deep-elbv2-3-app-cookie-stickiness-pins-with-cookie-spreads-without|_poll_until#19|Timeout|time.sleep(interval_s)
Issue-Key: elbv2.elbv2shape-deep-elbv2-3-least-outstanding-requests-starves-slow-backend-vs-round-robin|_poll_until#23|Timeout|time.sleep(interval_s)
Issue-Key: elbv2.elbv2shape-deep-elbv2-3-slow-start-with-least-outstanding-requests-rejected|_poll_until#18|Timeout|time.sleep(interval_s)
Issue-Key: elbv2.nlb-tcp-listener-preserves-client-source-ip-to-instance-target|_poll_until#24|Timeout|time.sleep(interval_s)
Issue-Key: elbv2.elbv2shape-deep-elbv2-3-oversize-request-line-and-headers-rejected-by-alb|_poll_until#17|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Key: elbv2.deep-elbv2-2-nlbwire-proxy-protocol-v2-signature-and-address-block|run#23|PublishedUrlRejected|_wait_dns(ctx, nlb["DNSName"], {ctx.endpoint_host(node["public_ip"], 80)[0]})
Issue-Key: elbv2.deep-elbv2-2-nlbwire-security-group-unlisted-listener-port-blocked|run#16|PublishedUrlRejected|_wait_dns(ctx, nlb["DNSName"], {ctx.endpoint_host(node["public_ip"], 80)[0]})
Issue-Key: elbv2.deep-elbv2-2-nlbwire-cross-zone-target-group-override|_resolve_v4#22|EgressRefused|infos = socket.getaddrinfo(host, None, socket.AF_INET, socket.SOCK_STREAM)
Issue-Key: elbv2.lb-cookie-stickiness-pins-a-client-to-one-target-in-the-group|_sample#19|AssertionError|assert status == 200, (
Issue-Key: elbv2.e2e-alb-target-routing-lifecycle|run#55|AssertionError|assert message == f"Function not found: {fn_arn}:$LATEST", f"MSG={message!r}"
Issue-Commit: 6e2d7185f30747a2356e601c1d412ac8e0be11c4

# elbv2: 5 net-new causes — the ALB/NLB data plane never answers at the addresses the emulator publishes, a healthy target is answered 502, and Lambda Invoke's not-found message drops the :$LATEST qualifier

> _Note: this issue was identified against the code base at commit `6e2d7185f30747a2356e601c1d412ac8e0be11c4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateListener

## Description
Five net-new root causes in the elbv2 suite, none of them carried by an open issue's replay key (the replay report lists all twelve check ids below as unowned and not blocked by an unimplemented stub).

=== Cause 1 of 5: an ELBv2 listener never serves a request on the endpoint the emulator publishes, so every data-plane behaviour is unobservable ===
Failing checks: elbv2.e2e-alb-traffic-lifecycle-flow, elbv2.elbv2attr-deep-elbv2-1-unhealthy-state-routing-min-healthy-count-serves-unhealthy-target, elbv2.elbv2shape-deep-elbv2-3-app-cookie-stickiness-pins-with-cookie-spreads-without, elbv2.elbv2shape-deep-elbv2-3-least-outstanding-requests-starves-slow-backend-vs-round-robin, elbv2.elbv2shape-deep-elbv2-3-slow-start-with-least-outstanding-requests-rejected, elbv2.nlb-tcp-listener-preserves-client-source-ip-to-instance-target, elbv2.elbv2shape-deep-elbv2-3-oversize-request-line-and-headers-rejected-by-alb
AWS: a listener created by CreateListener with a forward default action accepts client connections at the load balancer's endpoint and forwards them to the target group as soon as one target is healthy. Smithy (elasticloadbalancingv2): LoadBalancer.DNSName is documented "The public DNS name of the load balancer"; CreateLoadBalancer / CreateListener / RegisterTargets / DescribeTargetHealth / ModifyTargetGroupAttributes are the modeled control plane, and the target-group attributes these checks set (stickiness.type=app_cookie, stickiness.type=lb_cookie, load_balancing.algorithm.type=least_outstanding_requests, slow_start.duration_seconds, target health min-count fail-open, NLB client source-IP preservation) have no control-plane observable at all — their only effect is on that data path. The oversize request line / header case is answered by the load balancer itself (a 4xx generated before any target is reached). Recorded behavior: each of these checks drives the control plane to a healthy target group and then polls the published endpoint.
Emulator: no reply ever arrives on the published endpoint. Six of the checks are killed with "Check timed out after 60.0s" having never completed one request; elbv2.elbv2shape-deep-elbv2-3-oversize-request-line-and-headers-rejected-by-alb fails its readiness poll with "TimeoutError: http://<endpoint>/ping answering 200 (observed: []): the run budget ended first; not graded" — the observed-status list is empty, so not a single response was ever received.
Why: a client gets no answer at all where AWS serves traffic. Nothing that depends on the ALB/NLB forwarding path — stickiness cookies (AWSALB/AWSALBCORS, the app cookie), the least-outstanding-requests vs round-robin distribution, slow_start ramping, NLB source-IP preservation, the load-balancer-generated refusal of an oversize request line — can be exercised, and the whole ALB lifecycle check cannot reach a verdict.
Fix: the ELBv2 data plane that backs a listener (the listener/target-group forwarding path and its bind/announce), not the control-plane handlers, which all succeed in these runs. Related open issues describe the same symptom for other check ids but do not carry these: 20261116T083232Z-elbv2-2-net-new-causes-the-alb-data.md and 20261112T140934Z-elbv2-7-net-new-causes-loadbalancer-dnsname-is.md.
Traces: the seven check ids above.

=== Cause 2 of 5: an internet-facing NLB's own ENI publishes an Association.PublicIp that is not an address the emulator serves ===
Failing checks: elbv2.deep-elbv2-2-nlbwire-proxy-protocol-v2-signature-and-address-block, elbv2.deep-elbv2-2-nlbwire-security-group-unlisted-listener-port-blocked
AWS: an internet-facing network load balancer owns one elastic network interface per enabled Availability Zone, described "ELB net/<name>/<id>"; that ENI's Association.PublicIp is the address the load balancer's DNSName resolves to and the address that accepts connections on each listener port. Recorded behavior only for the address itself (the model documents DNSName as the public DNS name and does not fix the literal); the requirement is that the address the emulator announces is an address the emulator serves.
Emulator: DescribeNetworkInterfaces returns the NLB's ENI with a routable-looking public address that the emulator does not serve, so the connection cannot be made at all: "PublishedUrlRejected: endpoint_host was given 54.0.0.2:80 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]". 54.0.0.2 is this run's value, not part of the requirement — the defect is announcing any address outside the hosts the emulator is reached on.
Why: the published node address is the only handle a client has for an NLB's wire behaviour (PROXY protocol v2 header, per-listener-port security-group enforcement). A client that connects to the address AWS/the emulator published reaches nothing.
Fix: the component that mints the load balancer's own ENI addresses and the endpoint it announces for them, so an announced node address is one the emulator serves.
Traces: the two check ids above.

=== Cause 3 of 5: the NLB listener port announced for a node address is not served, so the connection is refused as off-endpoint ===
Failing check: elbv2.deep-elbv2-2-nlbwire-cross-zone-target-group-override
AWS: an NLB listener on port 80 accepts TCP connections at every zone's node address, and with load_balancing.cross_zone.enabled left at use_load_balancer_configuration each zone's entry point reaches its own zone's target. Recorded behavior only for the address/port mapping.
Emulator: the address/port it announces for the node is not the endpoint it serves — "CapabilityNotOffered: the target serves no address for 127.0.0.1:<port>" and "EgressRefused: egress refused: '127.0.0.1:<port>' is not the endpoint under test (127.0.0.1); ... the emulator announces an address other than the endpoint it is reached on". The port number is a per-run value; the fixed part of the finding is that the announced listener address/port is not served.
Why: cross-zone routing (and any NLB wire behaviour) cannot be reached: a client connecting to the announced NLB listener address gets nothing.
Fix: the NLB listener's bind/announce path — the listener port the emulator announces for a load balancer node must be the port it accepts connections on.
Traces: the check id above.

=== Cause 4 of 5: the ALB answers HTTP 502 for a target it reports healthy, so lb_cookie stickiness never happens ===
Failing check: elbv2.lb-cookie-stickiness-pins-a-client-to-one-target-in-the-group
AWS: with a target group whose stickiness.enabled=true and stickiness.type=lb_cookie, a request through the listener is forwarded to a healthy target and answered with the target's 200, and the load balancer mints the AWSALB and AWSALBCORS cookies (over a plain-HTTP listener AWSALBCORS carries SameSite=None without Secure); replaying AWSALB pins the same target, and an undecodable AWSALB is re-minted against a newly selected target. Recorded behavior: the check registers targets, waits for 'healthy', then samples the endpoint.
Emulator: the first request already fails — "request 1/30 to http://<endpoint>/ (headers=None) returned HTTP 502, expected 200; body=''". The emulator reports the target healthy and then answers a load-balancer-generated 502 with an empty body instead of forwarding, so no AWSALB/AWSALBCORS cookie is ever minted and no pinning can be observed.
Why: a client receives 502 Bad Gateway for a target the emulator itself says is healthy — the two planes contradict each other, and every stickiness guarantee built on the cookie is lost.
Fix: the listener forward action's target-selection/proxying path and its agreement with the health state DescribeTargetHealth reports; the lb_cookie cookie minting in the same path. Related open issue 20261112T140934Z-elbv2-7-net-new-causes-loadbalancer-dnsname-is.md describes a 502-for-registered-target cause for other check ids but does not carry this one.
Traces: the check id above.

=== Cause 5 of 5: Lambda Invoke's not-found message drops the resolved :$LATEST qualifier ===
Failing check: elbv2.e2e-alb-target-routing-lifecycle
AWS: Smithy (lambda) Invoke declares ResourceNotFoundException, errorKind client, httpError 404. For an invoke with no Qualifier, the message's fixed text is "Function not found: " followed by the fully qualified function ARN — the function's ARN taken from the request/state with the resolved qualifier appended, i.e. "<function-arn>:$LATEST". Fixed text: "Function not found: " and the ":$LATEST" suffix; variable part: the function ARN (partition, region, account, function name) from the request and the emulator's state.
Emulator: the status (404) and code (ResourceNotFoundException) are right, but the message stops at the bare ARN with no qualifier — observed 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>' where AWS words it 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>:$LATEST'.
Why: a client that parses or matches the qualified form (to recover which version was addressed, or to assert on the message) mis-handles the refusal; the message is part of the error contract.
Fix: the Invoke handler's ResourceNotFoundException construction — append the resolved qualifier to the ARN it formats into the message.
Traces: the check id above.

## Evidence
Cause 1 — expected: an HTTP/TCP response from the published load balancer endpoint once a target is healthy. Actual: "Check timed out after 60.0s" (6 checks) and "TimeoutError: http://<endpoint>/ping answering 200 (observed: []): the run budget ended first; not graded" — observed-status list empty, no response ever.
Cause 2 — expected: the NLB ENI's Association.PublicIp is an address the emulator serves on each listener port. Actual: "simparity.published.PublishedUrlRejected: endpoint_host was given 54.0.0.2:80 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address".
Cause 3 — expected: a TCP connection to the announced NLB node address:listener-port is accepted. Actual: "CapabilityNotOffered: the target serves no address for 127.0.0.1:35821" / "simparity.egress.EgressRefused: egress refused: '127.0.0.1:35821' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on".
Cause 4 — expected: HTTP 200 from the healthy target plus Set-Cookie AWSALB and AWSALBCORS. Actual: "AssertionError: request 1/30 to http://127.0.0.1:40095/ (headers=None) returned HTTP 502, expected 200; body=''" (host:port is this run's value).
Cause 5 — expected message: "Function not found: arn:aws:lambda:<region>:<account>:function:<name>:$LATEST" with HTTP 404 / ResourceNotFoundException. Actual: "Function not found: arn:aws:lambda:us-west-2:000000000000:function:sim2real-elbv2-fn-88ff589d83a6" — the ":$LATEST" suffix is missing (the ARN body itself is a per-run value).

## Steps to Reproduce
Cause 1: create an internet-facing ALB with two subnets, a target group with healthy HTTP targets, and an HTTP listener with a forward default action; set the target-group attribute under test (stickiness.type=app_cookie or lb_cookie, load_balancing.algorithm.type=least_outstanding_requests, slow_start.duration_seconds, or the target-health min-count fail-open), wait for DescribeTargetHealth 'healthy', then GET the load balancer's published endpoint. No response arrives. Same for an NLB TCP listener (nlb-tcp-listener-preserves-client-source-ip-to-instance-target) and for an oversize request line/headers.
Cause 2: create an internet-facing NLB across two subnets, then EC2 DescribeNetworkInterfaces with Filters Name=description Values=["ELB net/<lb-name>/<lb-id>"] and read Association.PublicIp; try to connect to that address on the listener port.
Cause 3: same NLB, leave the target group's load_balancing.cross_zone.enabled at 'use_load_balancer_configuration', register one target per zone, wait healthy, then open a TCP connection to each zone's node address on listener port 80.
Cause 4: create a target group with stickiness.enabled=true, stickiness.type=lb_cookie, register a target, wait for DescribeTargetHealth 'healthy', then issue a plain GET to the listener endpoint — the first request answers 502 with an empty body.
Cause 5: create a Lambda function, DeleteFunction it, then lambda Invoke(FunctionName=<name>, Payload=b"{}") and read Error.Message from the 404 ResourceNotFoundException.
