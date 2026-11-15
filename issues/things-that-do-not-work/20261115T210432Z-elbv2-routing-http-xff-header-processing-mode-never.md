Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elbv2.xff-header-processing-mode-changes-what-the-target-receives
Issue-Shape-Id: com.amazonaws.elasticloadbalancingv2#ElasticLoadBalancing_v10
Issue-Key: elbv2.xff-header-processing-mode-changes-what-the-target-receives|_poll_until#21|Timeout|last = probe()
Issue-Commit: 81a10bec8e4d8a5f3f1542243159208e3e9a2a27

# elbv2: routing.http.xff_header_processing.mode never changes the X-Forwarded-For header a Lambda target receives — the check reaches no verdict

> _Note: this issue was identified against the code base at commit `81a10bec8e4d8a5f3f1542243159208e3e9a2a27`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyLoadBalancerAttributes

## Description
Cause 1 of 1: the ALB data plane ignores the load balancer's stored routing.http.xff_header_processing.mode, and in fact never delivers any forwarded request to a lambda-type target group, so none of the three modes is observable and the check is killed before it can assert.

AWS: Smithy — ModifyLoadBalancerAttributes / DescribeLoadBalancerAttributes, LoadBalancerAttribute.Key documentation enumerates `routing.http.xff_header_processing.mode` as an Application-Load-Balancer-only attribute whose possible values are `append`, `preserve` and `remove`, default `append`. The model states the rule per value: with `append` the ALB "adds the client IP address (of the last hop) to the X-Forwarded-For header in the HTTP request before it sends it to targets" — so a client that supplied `X-Forwarded-For: <supplied-addr>` has the target see exactly two comma-separated elements, the supplied one first and the connecting client's own layer-3 address second (never a repeat of the supplied value); with `preserve` the ALB "preserves the X-Forwarded-For header in the HTTP request, and sends it to targets without any change", so the target sees the client-supplied value byte-for-byte; with `remove` the ALB "removes the X-Forwarded-For header in the HTTP request before it sends it to targets", so the target sees no X-Forwarded-For at all. The attribute governs X-Forwarded-For alone: X-Forwarded-Proto (the listener's protocol, e.g. `http` for an HTTP listener) and X-Forwarded-Port (the listener's port as a decimal string, e.g. `80` for a port-80 listener) are the same in all three modes. The attribute is accepted and echoed by ModifyLoadBalancerAttributes/DescribeLoadBalancerAttributes in every mode (HTTP 200, no error).

Emulator: recorded behavior only — no response is produced at all. The check is terminated with `Check timed out after 60.0s` and never reaches an assertion. The first probe in the check is a plain request through the port-80 HTTP listener whose default action forwards to a lambda-type target group holding the echoing function, polled until the target's echoed view contains `x-forwarded-for`; that probe never converges, so the subsequent per-mode legs (append, then preserve, then remove) are never exercised. The control plane calls succeed — the function reaches State=Active, RegisterTargets succeeds, DescribeTargetHealth lists the function ARN as a registered target, CreateListener returns a ListenerArn, and ModifyLoadBalancerAttributes/DescribeLoadBalancerAttributes read the attribute back — so the divergence is entirely on the forwarding path: the ALB never invokes the Lambda target and never returns its response, hence the X-Forwarded-For rewriting stage is never reached either.

Why: applications behind an ALB derive the client IP for authorization, rate limiting, geo rules and audit logging from X-Forwarded-For, and operators choose `preserve`/`remove` precisely to control whether a client-supplied value is trusted. Against this emulator a request to the load balancer never reaches the target, so a client gets no response where AWS returns the target's HTTP 200 body; and a test that sets the attribute and checks which header the target received validates nothing — `remove` appears indistinguishable from `append`, so a trust-the-client-XFF bug passes its emulator test and ships.

Fix: the elbv2 data plane. The ALB request-forwarding path must (a) actually dispatch a matched forward action into a lambda-type target group — invoking the registered function with the ALB event shape and returning its statusCode/headers/body to the client — and (b) before dispatch, read the load balancer's stored `routing.http.xff_header_processing.mode` and append the connecting peer's layer-3 address to / pass through / delete the inbound X-Forwarded-For header accordingly, leaving X-Forwarded-Proto and X-Forwarded-Port derived from the listener protocol and port in all three modes. The attribute's storage and echo in ModifyLoadBalancerAttributes/DescribeLoadBalancerAttributes already behave; only the forwarding path needs it.

Related open issues, named for the fixer but which do NOT carry this check's replay key (the suite replayed this check and it is still failing, unowned): 20261111T030957Z-elbv2-2-net-new-causes-routing-http-desync.md describes this same attribute as inert, and 20261111T195612Z-elbv2-the-source-ip-listener-rule-check-reaches.md describes an ALB never answering a forward rule into a Lambda target group.

Traces: elbv2.xff-header-processing-mode-changes-what-the-target-receives.

## Evidence
Failing check: [mismatch] elbv2.xff-header-processing-mode-changes-what-the-target-receives
Reported detail: `Error: Check timed out after 60.0s` — no assertion was reached; the check was killed inside its poll loop.

Expected (AWS), for an internet-facing ALB with a port-80 HTTP listener whose default action forwards to a lambda-type target group holding an echoing function, each request carrying `X-Forwarded-For: <supplied-addr>`:
- HTTP 200 with the function's body in every mode (the invocation happens).
- routing.http.xff_header_processing.mode=append  -> target sees `x-forwarded-for: <supplied-addr>, <this client's own address>` (exactly 2 elements, element 1 == the supplied value, element 2 != the supplied value).
- ...=preserve -> target sees `x-forwarded-for: <supplied-addr>` byte-for-byte, with nothing appended.
- ...=remove   -> target sees no `x-forwarded-for` header at all.
- In all three modes: `x-forwarded-proto` == the listener protocol lowercased (`http` here) and `x-forwarded-port` == the listener port as a decimal string (`80` here). (`<supplied-addr>`, the client address, the listener port and the function/LB names are request/state values, not fixed text.)

Actual (emulator): no forwarded request is ever observed at the target in any mode — the very first probe, taken before any attribute is set, never yields an echoed header set, so the load balancer produced no target response at all and the check timed out at 60 s with zero verdicts. Control plane is healthy around it: CreateLoadBalancer/CreateTargetGroup(TargetType=lambda)/RegisterTargets/CreateListener all succeed, DescribeTargetHealth lists the function ARN as registered, and ModifyLoadBalancerAttributes + DescribeLoadBalancerAttributes accept and read back routing.http.xff_header_processing.mode for append, preserve and remove.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite so the emulator is serving.
2. Create an internet-facing Application Load Balancer in the shared VPC with two AZ subnets and a security group allowing TCP/80 ingress; wait for it to become active.
3. Create a Lambda function that returns {"statusCode": 200, "headers": {...}, "body": json.dumps({"headers": event["headers"]})} and wait for State=Active.
4. CreateTargetGroup with TargetType=lambda, grant elasticloadbalancing.amazonaws.com lambda:InvokeFunction on it, and RegisterTargets with the function ARN. Confirm DescribeTargetHealth lists the function ARN.
5. CreateListener on Port=80 Protocol=HTTP with a default forward action to that target group.
6. GET http://<LB DNSName>/ with header `X-Forwarded-For: 203.0.113.7` and inspect the echoed header set. Observe that no 200/echo body is returned at all (this already diverges).
7. For each value in append, preserve, remove: ModifyLoadBalancerAttributes with Key=routing.http.xff_header_processing.mode, poll DescribeLoadBalancerAttributes until it reads back, then repeat the GET of step 6 and compare the target's view of x-forwarded-for (and x-forwarded-proto / x-forwarded-port) against the per-mode expectations above.
