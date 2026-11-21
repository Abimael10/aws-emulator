Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elbv2.target-health-check-state-transition-unhealthy-to-healthy elbv2.routing-http2-attribute-decides-the-alpn-protocol-the-alb-selects elbv2.op-contract-delete-listener elbv2.e2e-blue-green-weighted-target-group-shift elbv2.e2e-http-to-https-redirect-traffic-flow elbv2.listener-ssl-policy-bounds-the-negotiated-tls-version-and-cipher elbv2.elbv2attr-deep-elbv2-1-mtls-passthrough-clientcert-chain elbv2.elbv2attr-deep-elbv2-1-mtls-passthrough-forged-clientcert-header-stripped elbv2.elbv2attr-deep-elbv2-1-server-header-removed-and-hsts-injected
Issue-Shape-Id: com.amazonaws.elasticloadbalancingv2#ElasticLoadBalancing_v10
Issue-Key: elbv2.target-health-check-state-transition-unhealthy-to-healthy|run#14|AssertionError|assert state1 == "unhealthy", f"expected unhealthy before the code fix, got {state1}"
Issue-Key: elbv2.routing-http2-attribute-decides-the-alpn-protocol-the-alb-selects|run#14|AssertionError|assert selected == "h2", (
Issue-Key: elbv2|DeleteListener|ListenerNotFound
Issue-Key: elbv2.e2e-blue-green-weighted-target-group-shift|_poll_http_get#23|Timeout|time.sleep(interval_s)
Issue-Key: elbv2.e2e-http-to-https-redirect-traffic-flow|_poll_http_get#21|Timeout|time.sleep(interval_s)
Issue-Key: elbv2.listener-ssl-policy-bounds-the-negotiated-tls-version-and-cipher|_poll_until#24|Timeout|time.sleep(interval_s)
Issue-Key: elbv2.elbv2attr-deep-elbv2-1-mtls-passthrough-clientcert-chain|_poll_until#29|Timeout|time.sleep(interval_s)
Issue-Key: elbv2.elbv2attr-deep-elbv2-1-mtls-passthrough-forged-clientcert-header-stripped|_poll_until#28|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Key: elbv2.elbv2attr-deep-elbv2-1-server-header-removed-and-hsts-injected|_poll_until#27|Timeout|time.sleep(interval_s)
Issue-Commit: 5d0d11832

# elbv2: four target-health, ALPN, listener-lifecycle and data-plane causes still open (split out of the seven-cause report)

> _Split out of `issues/things-that-now-work/20261104T094656Z-elbv2-seven-target-health-routing-and-data-plane-causes-still-open.md`, whose causes 3, 6, 8 and 10 and the SCHEDULE half of cause 2 are fixed. They kept the original report's numbering, trace ids and Issue-Keys so they stay filed and replayable._
>
> _RESOLVED in part: cause 7 (`DeleteListener` on a listener this API has already deleted) and the MUTUAL TLS half of cause 9 are fixed, and the SUBJECT of cause 4 (the application protocols a TLS-terminating listener offers, and the HTTP/2 it then speaks) and of the first half of cause 9.3 (the response headers a listener's attributes decide) are implemented and pinned by `tests/all/alb_negotiated_application_protocol.rs` and `tests/all/alb_listener_response_headers.rs`. The causes that do NOT pass — cause 2, the poll ahead of cause 4's assertion, the `{DNSName}:{listener port}` addressing of cause 9.1 and the two remaining polls of cause 9.3 — are RE-FILED with their six replay keys in `issues/things-that-do-not-work/20261118T090000Z-elbv2-target-health-on-silence-alpn-poll-ssl-policy-and-dnsname-port-addressing-remain-open.md`, so they are not recorded as working. The descriptions below are the state of the code base BEFORE that work; read the re-filed report for what is left._

## Operation
DescribeTargetHealth

## Description
Four of the original ten ELBv2 root causes remain open. Each is independent and must be closed separately; each entry states what was learned while the others were fixed, so the next fixer does not start from zero.

Cause 2 of 10 (what is LEFT of the probe half): a registration this emulator can observe NOTHING about keeps the health the clock owes it, and one recorded check expects `unhealthy` for such a registration.
AWS: recorded behavior only: after the `initial` state a target becomes `healthy` or `unhealthy` according to the configured health check.
Emulator: the state machine, the CADENCE of the checks (consecutive checks one `HealthCheckIntervalSeconds` apart, so a verdict is as many intervals away as the group's threshold takes checks) and what one check observes are all real now (`src/state/target_health.rs`, `src/state/host_counts.rs`): a check of an `ip` target passes while some network interface of the account holds the address and fails for one no interface holds, an `instance` target passes while EC2 reports it running, a check pointed at a port the workload does not serve fails, and a check the target's security groups admit no connection to fails. A target type this service cannot read — and a target group whose VPC the account's EC2 inventory does not hold — is still observed as nothing at all, and a registration nothing was observed about keeps `healthy` rather than being failed on silence.
What is known: this check registers a target the emulator holds something behind, or none it can read, and then expects the health check ITSELF to fail — which is a request to a path, matched against the group's `Matcher`, answered by a workload. A connection to an arbitrary registered address is egress, which this project forbids and the merge gate's pattern scan refuses, and a constant `unhealthy` is the same defect as a constant `healthy` with another literal. The one target type this emulator can really answer a layer-7 check for is `lambda`, whose function the Lambda service holds and `src/service/lambda_target.rs` already invokes in process for a `forward`. The check's own step sequence is needed to say which target type it registers.
Traces: elbv2.target-health-check-state-transition-unhealthy-to-healthy.

Cause 4 of 10: `routing.http2.enabled` is not honored in ALPN selection.
AWS: recorded behavior only: with the load-balancer attribute `routing.http2.enabled=true` an HTTPS client offering ALPN `['h2','http/1.1']` negotiates `h2`.
Emulator: the check never reaches that assertion — it exhausts its 60 s budget in the poll before it.
What is known: the attribute is stored and reported by the control plane, and the TLS handshake of an HTTPS listener is this service's own now (`src/service/listener_socket.rs` builds a `rustls` server configuration through `service_common::acm_certificate_termination::server_config`), so the protocol list IS this service's to offer — that shared constructor takes no ALPN argument yet, and the socket's request path speaks HTTP/1.1 only (`hyper` with the `http1` feature). Offering `h2` without serving HTTP/2 on that socket would break every HTTPS check that passes today, so the two have to land together. Why the poll before the assertion times out is not known; that check's step sequence is needed.
Traces: elbv2.routing-http2-attribute-decides-the-alpn-protocol-the-alb-selects.

Cause 7 of 10: DeleteListener answers ListenerNotFound for a listener the same run created.
AWS: Smithy: DeleteListener errors are ListenerNotFoundException and ResourceInUseException; a listener that exists is deleted with HTTP 200 and an empty body.
Emulator: `ListenerNotFound` — "Listener '<listener-arn>' not found" — for a listener ARN of the shape `CreateListener` mints.
What is known: it is NOT the cascade (the recorded `cascade-delete-dependency` check asserts that a listener removed with its load balancer IS refused `ListenerNotFound`, and that is what the emulator answers), and it is NOT an ARN-form mismatch: a CreateListener/DeleteListener round trip resolves, pinned by `tests/all/listener_lifecycle_certificates_and_rules.rs` and `tests/all/cascaded_delete_idempotency.rs`. Something else in that run removed the listener first; the check's own call sequence is needed.
Traces: elbv2.op-contract-delete-listener.

Cause 9 of 10: the ALB/NLB listener data plane does not answer every client request.
AWS: recorded behavior only: a listener on a load balancer whose target group has a registered, healthy target answers a client request — over HTTP, over HTTPS with the listener's certificate and SNI selection, with mTLS passthrough headers, with X-Forwarded-For processing and with weighted/sticky forwarding.
Emulator: four of the ten checks of this cause pass now (`elbv2.e2e-alb-target-routing-lifecycle`, `elbv2.imported-certificate-is-presented-and-sni-selects-per-host`, `elbv2.xff-header-processing-mode-changes-what-the-target-receives`, `elbv2.source-ip-condition-uses-the-layer3-address-not-x-forwarded-for`): the HTTP and the HTTPS halves of the plane exist, a `fixed-response`, a `redirect` and a `forward` to a `lambda` target are served out of stored records (`src/service/data_plane.rs`, `src/service/lambda_target.rs`), and the listener terminates TLS under the certificate Certificate Manager holds for it. Six are still not answered, in three distinct ways:
1. ADDRESSING. `elbv2.e2e-blue-green-weighted-target-group-shift` and `elbv2.e2e-http-to-https-redirect-traffic-flow` compose their request URL as `http://{DNSName}:{listener port}` and are refused before any connection: the published `DNSName` is the `host:port` of the load balancer's own core listener (`src/state/load_balancer_endpoint.rs`), so the composed value is `http://127.0.0.1:34321:80`, which is not a URL with a host. A client that appends the listener's port to the published name — which is what a client handed a real `DNSName` does — cannot reach the plane at all. Closing it needs a core seam: the service cannot choose the port its listener answers on (`vendor/server/src/listeners.rs`'s `serve_native` binds the interface and port range the environment fixes), and a name under Elastic Load Balancing's own zone resolves nowhere on the machine, so neither a published authority nor a published hostname serves such a client today.
2. MUTUAL TLS. `elbv2.elbv2attr-deep-elbv2-1-mtls-passthrough-clientcert-chain` and `...-forged-clientcert-header-stripped` now reach the target and assert on headers: the target received no `x-amzn-mtls-clientcert`. The listener's handshake asks for no CLIENT certificate (`src/service/listener_socket.rs` passes no client-certificate verifier to `service_common::acm_certificate_termination::server_config`), so no chain exists to pass through, and nothing strips a client-sent `x-amzn-mtls-*` header off a forwarded request. Both are this service's to implement, against the listener's `MutualAuthentication` mode.
3. STILL UNANSWERED. `elbv2.listener-ssl-policy-bounds-the-negotiated-tls-version-and-cipher` and `elbv2.elbv2attr-deep-elbv2-1-server-header-removed-and-hsts-injected` exhaust their budget in a poll. Their step sequences are needed to say what they poll for.
Traces: elbv2.e2e-blue-green-weighted-target-group-shift, elbv2.e2e-http-to-https-redirect-traffic-flow, elbv2.listener-ssl-policy-bounds-the-negotiated-tls-version-and-cipher, elbv2.elbv2attr-deep-elbv2-1-mtls-passthrough-clientcert-chain, elbv2.elbv2attr-deep-elbv2-1-mtls-passthrough-forged-clientcert-header-stripped, elbv2.elbv2attr-deep-elbv2-1-server-header-removed-and-hsts-injected.

## Steps to Reproduce
Replay the behavioral suite and re-run exactly the checks in trace_ids.
1. Cause 2 — register a target whose health check cannot be answered, wait past the settling span and poll DescribeTargetHealth; expect 'unhealthy' with a Reason.
2. Cause 4 — set routing.http2.enabled=true, connect to the HTTPS listener offering ALPN ['h2','http/1.1']; expect 'h2'.
3. Cause 7 — CreateListener then DeleteListener with the returned ARN, in the op-contract check's own call sequence; expect HTTP 200.
4. Cause 9 — for each listed check, bring up the listener and target as the check does and send the client request; expect a response within the check budget.
