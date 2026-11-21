Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elbv2.target-health-check-state-transition-unhealthy-to-healthy elbv2.routing-http2-attribute-decides-the-alpn-protocol-the-alb-selects elbv2.e2e-blue-green-weighted-target-group-shift elbv2.e2e-http-to-https-redirect-traffic-flow elbv2.listener-ssl-policy-bounds-the-negotiated-tls-version-and-cipher elbv2.elbv2attr-deep-elbv2-1-server-header-removed-and-hsts-injected
Issue-Shape-Id: com.amazonaws.elasticloadbalancingv2#ElasticLoadBalancing_v10
Issue-Key: elbv2.target-health-check-state-transition-unhealthy-to-healthy|run#14|AssertionError|assert state1 == "unhealthy", f"expected unhealthy before the code fix, got {state1}"
Issue-Key: elbv2.routing-http2-attribute-decides-the-alpn-protocol-the-alb-selects|run#14|AssertionError|assert selected == "h2", (
Issue-Key: elbv2.e2e-blue-green-weighted-target-group-shift|_poll_http_get#23|Timeout|time.sleep(interval_s)
Issue-Key: elbv2.e2e-http-to-https-redirect-traffic-flow|_poll_http_get#21|Timeout|time.sleep(interval_s)
Issue-Key: elbv2.listener-ssl-policy-bounds-the-negotiated-tls-version-and-cipher|_poll_until#24|Timeout|time.sleep(interval_s)
Issue-Key: elbv2.elbv2attr-deep-elbv2-1-server-header-removed-and-hsts-injected|_poll_until#27|Timeout|time.sleep(interval_s)
Issue-Commit: 4f97f681847766e648d6308f136db847a49a92f6

# elbv2: target health on silence, the three HTTPS checks that time out in a poll, and the `{DNSName}:{listener port}` addressing remain open

> _Split out of `issues/things-that-now-work/20261116T120000Z-elbv2-four-target-health-alpn-listener-lifecycle-and-data-plane-causes-still-open.md`, whose cause 7 (`DeleteListener` on a listener this API already deleted) is fixed, and whose cause 4 and the first half of cause 9.3 are now IMPLEMENTED although their checks still do not pass. These six replay keys do NOT pass, and are re-filed here so they are not recorded as working. Everything below is what the earlier fixes established about them; a later fixer starts from here._

## Operation
DescribeTargetHealth

## Description
Four independent causes remain. Each entry says what is already real in the code base, so nothing is traced twice.

Cause A (the parent's cause 2 of 10): a registration this emulator can observe NOTHING about keeps `healthy`, where the recorded check expects `unhealthy`.
AWS: recorded behavior only: after the `initial` state a target becomes `healthy` or `unhealthy` according to the configured health check; `HealthyThresholdCount` is "the number of consecutive health check successes required before considering an unhealthy target healthy", so a target nothing has answered for has not earned `healthy`.
Emulator: the state machine, the CADENCE (`src/state/target_health.rs`, `src/state/host_counts.rs`) and what one check observes are real for every target type this service can read — an `ip` target fails while no interface of the account holds the address, an `instance` target fails while EC2 reports it not running, a check pointed at a port the workload does not serve fails, a check the target's security groups admit no connection to fails, one that would have to cross networks the group does not reach fails, and a `lambda` check is the function's own invocation. What is left is SILENCE: `host_counts::observe` answers `None` for a target group whose VPC the account's EC2 inventory does not hold and for a target type this service does not read, and `TargetGroupRecord::observed_check` then keeps the verdict the registration started with, which `health_of` reports as `healthy` once the settling span has passed.
What is known: the narrow change (treat silence as a check nothing answered) flips the reported health of EVERY registration whose VPC the EC2 inventory does not hold — the configuration most of this crate's own suites and several other services' suites create, and the one behind the passing checks `elbv2.e2e-alb-target-routing-lifecycle`, `elbv2.deploy-alb-cfn-then-target-health-behaves` and `elbv2.deploy-nlb-target-registration-cfn-health-check` — so it cannot be made without the check's own step sequence (which target type it registers, and its health-check configuration) and a replay wide enough to show the collateral damage. Note also that a verdict is `UnhealthyThresholdCount` intervals away at the group's own configured cadence, so a group at the recorded interval of 30 s with a threshold of 3 cannot reach any verdict inside the harness's 60 s budget: whether `HealthCheckIntervalSeconds` keeps its configured value (it does today, with the member cited on the constant) or is compressed like a transition span is part of this cause.
Related open issues on the same machinery, which do not carry these keys: `20261117T061022Z-ec2-elbv2-ip-type-alb-targets-never-leave.md` (the same rule for `ip` targets, with the recorded configuration written out) and `20261111T030957Z-autoscaling-ec2-elbv2-4-net-new-causes-target.md`.
Traces: elbv2.target-health-check-state-transition-unhealthy-to-healthy.

Cause B (the parent's cause 4 of 10): the ALPN check never reaches its assertion.
AWS: recorded behavior only: with `routing.http2.enabled=true` an HTTPS client offering ALPN `['h2','http/1.1']` negotiates `h2`.
Emulator: the SUBJECT is implemented now and pinned end to end by `tests/all/alb_negotiated_application_protocol.rs`: a TLS-terminating listener offers the protocols its own configuration names (`src/service/alpn_protocols.rs` — a TLS listener's `AlpnPolicy`, and the load balancer's `routing.http2.enabled` for an Application Load Balancer's HTTPS listener), the handshake selects the first of them the client offered, and the socket serves HTTP/2 on a connection that negotiated `h2` (`src/service/listener_socket.rs`). The check still exhausts its 60 s budget in `_poll_until#13`, which runs BEFORE the `selected == "h2"` assertion; what that poll waits for is not visible in the replay output.
Traces: elbv2.routing-http2-attribute-decides-the-alpn-protocol-the-alb-selects.

Cause C (the parent's cause 9.1): a client that appends the listener's port to the published `DNSName` cannot reach the plane at all.
AWS: recorded behavior only: a client composes `http://{DNSName}:{listener port}` and the listener answers it.
Emulator: `EndpointRefused: endpoint 'http://127.0.0.1:39783:80' is not a http/https/ws/wss/rtmp/rtmps URL with a host`. `DNSName` must be an address a client on this machine can dial, so it is the `host:port` of the core listener the load balancer opened (`src/state/load_balancer_endpoint.rs`); appending a port to it is not a URL. Publishing a name in Elastic Load Balancing's own zone instead is the defect `20261109T143515Z-cloudwatch-ecs-elbv2-logs-an-alb-s-dnsname-is-published-as-name.md` filed (that name resolves to nothing and the harness refuses it), and publishing a bare loopback host would make one host the `DNSName` of every load balancer, so the data plane could resolve none of them.
Why it is BLOCKED, and on what: serving the authority the client composes needs the service to choose the PORT its listener answers on. The core owns that: `vendor/server/src/listeners.rs:284` `serve_native` binds `bind_ip_from_env()` and `listener_port_range_from_env()` (line 291), the interface and port range the environment fixes, and `ResourceListener` exposes no way to ask for a port. The core change this needs is a `tcp_plaintext`-style opener that takes a REQUESTED port (a listener's own 80/443) and reports whether it was granted, so a load balancer can publish a host whose port is the listener's.
Traces: elbv2.e2e-blue-green-weighted-target-group-shift, elbv2.e2e-http-to-https-redirect-traffic-flow.

Cause D (the parent's cause 9.3, second half): two more HTTPS checks exhaust their budget in a poll.
AWS: recorded behavior only: a listener's security policy bounds the TLS version and cipher a client negotiates; a listener whose attributes remove the `Server` header and set a `Strict-Transport-Security` value answers accordingly.
Emulator: both subjects are real now — the security policy's `SslProtocols` bound the versions a handshake completes under (`listener_socket::negotiated_versions`, pinned by `tests/all/alb_https_listener_tls.rs`), and the listener's `routing.http.response.*` attributes decide the headers of every answer (`src/service/response_headers.rs`, pinned by `tests/all/alb_listener_response_headers.rs`). Both checks still time out in a poll that precedes their assertions (`_poll_until#17` and `_poll_until#27`), as the ALPN check does, so all three are likely blocked on one thing the three fixtures share — and the two mTLS checks of the same `elbv2attr-deep-elbv2-1` fixture family DO pass, so it is not that fixture's addressing. Their step sequences are what is needed.
Traces: elbv2.listener-ssl-policy-bounds-the-negotiated-tls-version-and-cipher, elbv2.elbv2attr-deep-elbv2-1-server-header-removed-and-hsts-injected.

## Steps to Reproduce
Replay the behavioral suite and re-run exactly the checks in trace_ids.
1. Cause A — register a target whose health check this service can observe nothing about, wait past the settling span and poll DescribeTargetHealth; expect 'unhealthy' with a Reason, and observe 'healthy'.
2. Cause B — set routing.http2.enabled=true, connect to the HTTPS listener offering ALPN ['h2','http/1.1']; the handshake selects 'h2' today, so find what the poll ahead of that assertion waits for.
3. Cause C — read DNSName and request `http://{DNSName}:{listener port}/`; expect the listener's answer.
4. Cause D — bring up the listener as each check does and send the client request; expect a response within the check budget.
