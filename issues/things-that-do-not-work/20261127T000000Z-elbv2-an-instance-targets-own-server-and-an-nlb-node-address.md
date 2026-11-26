Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elbv2.elbv2attr-deep-elbv2-1-unhealthy-state-routing-min-healthy-count-serves-unhealthy-target elbv2.elbv2shape-deep-elbv2-3-app-cookie-stickiness-pins-with-cookie-spreads-without elbv2.elbv2shape-deep-elbv2-3-least-outstanding-requests-starves-slow-backend-vs-round-robin elbv2.elbv2shape-deep-elbv2-3-slow-start-with-least-outstanding-requests-rejected elbv2.nlb-tcp-listener-preserves-client-source-ip-to-instance-target elbv2.elbv2shape-deep-elbv2-3-oversize-request-line-and-headers-rejected-by-alb elbv2.deep-elbv2-2-nlbwire-proxy-protocol-v2-signature-and-address-block elbv2.deep-elbv2-2-nlbwire-security-group-unlisted-listener-port-blocked elbv2.deep-elbv2-2-nlbwire-cross-zone-target-group-override elbv2.lb-cookie-stickiness-pins-a-client-to-one-target-in-the-group
Issue-Shape-Id: com.amazonaws.elasticloadbalancingv2#ElasticLoadBalancing_v10
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
Issue-Commit: 05cd0f72caf1a731969632cd11c089950884fedd

# elbv2: an `instance` target's own server is never behind its registration (so a forward answers 502 or nothing), and an NLB node's announced address and listener port are not ones this emulator serves

> _Note: this issue carries the three causes of `20261119T043009Z-elbv2-5-net-new-causes-the-alb-nlb.md` that the target-selection fix of that issue did NOT close. That issue's cause 4 (the forward's target selection and its `lb_cookie` / `app_cookie` pin minting) and cause 5 (Lambda `Invoke`'s `:$LATEST` message) are done; everything below is what remains, re-filed so it stays open rather than being carried by a closed report._

## Operation
RegisterTargets

## Description

=== Cause 1 of 2: nothing of this emulator stands a SERVER behind an `instance` (or `ip`) registration, so a `forward` to it answers the load balancer's own 502 — or never answers the readiness poll in front of the behaviour under test ===
Failing checks: elbv2.lb-cookie-stickiness-pins-a-client-to-one-target-in-the-group, elbv2.elbv2shape-deep-elbv2-3-app-cookie-stickiness-pins-with-cookie-spreads-without, elbv2.elbv2shape-deep-elbv2-3-least-outstanding-requests-starves-slow-backend-vs-round-robin, elbv2.elbv2shape-deep-elbv2-3-slow-start-with-least-outstanding-requests-rejected, elbv2.elbv2shape-deep-elbv2-3-oversize-request-line-and-headers-rejected-by-alb, elbv2.elbv2attr-deep-elbv2-1-unhealthy-state-routing-min-healthy-count-serves-unhealthy-target, elbv2.nlb-tcp-listener-preserves-client-source-ip-to-instance-target.

AWS: `TargetGroup$TargetType` gives what such a registration names — "instance - Targets are specified by instance ID", "ip - Targets are specified by IP address" — and `TargetDescription$Port` where the workload behind it is reached ("the port on which the target is listening"). A `forward` to a target the load balancer may send requests to is answered with the TARGET's own status and bytes; a load-balancer-generated 502 is reserved for a target it could not get a well-formed response from. Every behaviour these checks exist to grade (the per-target stickiness pins, the least-outstanding-requests vs round-robin spread, `slow_start`, the target-group-health fail-open serving an unhealthy target, an NLB's client source-IP preservation, and the load balancer's own refusal of an oversize request line) is layered on that one answer.

Emulator: an instance of this account runs only what `services/service-com-amazonaws-ec2-amazonec2/src/state/guest_services.rs` models — a distribution's web-server package on its OWN shipped port, and an interpreter's `-m http.server` — and these checks' targets are behind neither, so `crate::state::target_servers::answered` resolves nothing for the registration and `crate::service::instance_target` answers `TARGET_UNREACHABLE_STATUS`. Observed: `AssertionError: request 1/30 to http://127.0.0.1:<port>/ (headers=None) returned HTTP 502, expected 200; body=''` on the very first request, `TimeoutError: http://127.0.0.1:<port>/ping answering 200 (observed: [])` with no response ever recorded, and six further checks cut at the 60 s harness cap. The host, the port and the body markers are per-run values; the constant is the 502 / the silence.

Why: the two planes contradict each other — `DescribeTargetHealth` reports the registration `healthy` (nothing is failed on silence) while a client is answered 502 for it — so a healthy-looking deployment serves a gateway error, and no routing behaviour of an `instance` target group can be observed at all.

Fix: the capability, not the symptom — an instance's guest must RUN the server its own user data starts, on the port the registration receives traffic on, so `crate::state::target_servers::answered` resolves it for both the health check and the forward (the two already read it one way). That is EC2's own surface (`guest_services.rs` beside `guest_console.rs`), and the ELBv2 side needs no change once a server is there. Related open issues carrying the same 502 for other check ids: `20261123T141857Z-elbv2-4-net-new-causes-an-alb-answers.md` (cause 1), `20261110T143849Z-ec2-elbv2-ssm-the-guest-driven-alb-target.md`, `20261122T231140Z-cloudwatch-elbv2-s3-elbv2-the-alb-answers-http-502-for-a.md`.
Traces: the seven check ids above.

=== Cause 2 of 2: an NLB node's announced address, and the listener port announced for it, are not ones this emulator serves ===
Failing checks: elbv2.deep-elbv2-2-nlbwire-proxy-protocol-v2-signature-and-address-block, elbv2.deep-elbv2-2-nlbwire-security-group-unlisted-listener-port-blocked, elbv2.deep-elbv2-2-nlbwire-cross-zone-target-group-override.

AWS: an internet-facing network load balancer owns one elastic network interface per enabled Availability Zone, described `ELB net/<name>/<id>`; that interface's `Association.PublicIp` is the address the load balancer's `DNSName` resolves to and the address that accepts connections on each LISTENER port, and `LoadBalancer$DNSName` is "the public DNS name of the load balancer" — a name a client resolves. Recorded behaviour only for the address itself; the requirement is that an address the emulator announces for a node is an address the emulator serves, on the listener's own port.

Emulator: the node's interface carries an Amazon-assigned public address out of EC2's own pool (`services/service-com-amazonaws-ec2-amazonec2/src/state/elastic_ips_and_transit_gateway_route_tables.rs`, drawn from `54.0.0.0/8`), which nothing here answers on — `PublishedUrlRejected: endpoint_host was given 54.0.0.2:80 … its host is not one of the emulator's own hosts` — and `DNSName` is the `host:port` of the socket the load balancer opened, which resolves nowhere: `EgressRefused: egress refused: '127.0.0.1:<port>' is not the endpoint under test (127.0.0.1)` out of `socket.getaddrinfo`. The address, the port and the run's own host are per-run values; the constant is that neither the node address nor the listener port announced for it is served.

Why: the node address is the only handle a client has on an NLB's wire behaviour (the PROXY protocol v2 header, per-listener-port security-group enforcement, a zone's own entry point for cross-zone routing). A client that connects to the address the emulator published reaches nothing.

Fix: this needs a core listener a service can open on the ADDRESS and PORT it must publish. `vendor/server/src/listeners.rs` (`Listeners::tcp_plaintext`, `Listeners::tcp`) binds `bind_ip_from_env()` and a port out of `listener_port_range_from_env()` — both the operator's — and `ResourceListener` reports only what was assigned, so a service cannot serve `<node address>:80` and announcing it would announce an address nothing answers on. Until the core takes a requested address and port, the honest state of these three checks is blocked rather than refused. The `DNSName` half is also carried by `20261122T170634Z-ec2-elbv2-createloadbalancer-describeloadbalancers-publish-dnsname-as-a.md`.
Traces: the three check ids above.

## Evidence
Cause 1 — expected: the target's own HTTP 200 and bytes once a registration is one the load balancer may send requests to. Actual: `request 1/30 … returned HTTP 502, expected 200; body=''`; `http://127.0.0.1:<port>/ping answering 200 (observed: [])`; `Check timed out after 60.0s` for the remaining six.
Cause 2 — expected: the NLB ENI's `Association.PublicIp` is an address the emulator serves on each listener port, and `DNSName` resolves to it. Actual: `PublishedUrlRejected: endpoint_host was given 54.0.0.2:80 …`; `EgressRefused: egress refused: '127.0.0.1:<port>' is not the endpoint under test (127.0.0.1)`.

## Steps to Reproduce
1. Cause 1: create an internet-facing ALB across two subnets, a target group (`TargetType=instance`, `Protocol=HTTP`, an HTTP health check), launch an EC2 instance whose user data brings up an HTTP server on the traffic port, `RegisterTargets`, wait for `DescribeTargetHealth` `healthy`, then `GET http://{DNSName}/` — expect the target's 200 and its bytes, observe `(502, '')`.
2. Cause 2: create an internet-facing NLB across two subnets, then EC2 `DescribeNetworkInterfaces` with `Filters Name=description Values=["ELB net/<lb-name>/<lb-id>"]`, read `Association.PublicIp`, and open a TCP connection to that address on listener port 80; also resolve `DNSName` as a host name.
