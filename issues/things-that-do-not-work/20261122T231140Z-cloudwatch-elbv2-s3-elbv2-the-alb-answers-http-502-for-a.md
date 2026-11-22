Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__elbv2__s3.e2e-diagnose-alb-five-xx-errors-lifecycle
Issue-Key: cloudwatch__elbv2__s3.e2e-diagnose-alb-five-xx-errors-lifecycle|phase#9|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Commit: a9b1f91879b3ed1957cfd9e0361182ba83602a4e

# elbv2: the ALB answers HTTP 502 for a routed GET /health to the Fargate ip target it has just reported healthy, so the ALB 5xx diagnosis lifecycle never gets past deploy

> _Note: this issue was identified against the code base at commit `a9b1f91879b3ed1957cfd9e0361182ba83602a4e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ALB HTTP data plane (listener-rule forward action to a registered target)

## Description
Cause 1 of 1: after the estate's Fargate task is registered and DescribeTargetHealth reports it `healthy` on port 8080, and after DescribeLoadBalancers reports the load balancer `active`, a routed GET /health through the ALB is answered HTTP 502 with an empty body — forever. The request matches the listener rule (it carries the X-Sim2real-Run header the rule conditions on) whose single action forwards to that target group, so the emulator is choosing the target and then failing to serve it.

AWS: recorded behavior only for the data path. The estate's target group is TargetType ip, Port 8080, HealthCheckPath /health, and the task's container runs a node HTTP server that answers `/health` with 200 and the body "ok". On AWS, once the ALB is active and the task is a healthy registered target, a request matching the forward rule reaches the container and the viewer gets the container's own 200/"ok". AWS reserves HTTP 502 (ELB's own error, logged with elb_status_code 502) for a target that answered with a malformed or truncated response, and 503 for a rule that chose a target group with no healthy target — so a healthy target running that server cannot be answered 502 twice in a row, let alone for 180 s.

Emulator: every warm-up GET /health through the load balancer's published address answers (502, '') until the check's 180 s poll gives up: `AssertionError: [phase 1/5 deploy] a routed GET /health through '<alb address>' answering 200 not met within 180s; last (502, '')`. The two preceding observations passed, so this is not a provisioning or registration gap: the target group's single target IS reported TargetHealth.State == 'healthy' on Port 8080, and the load balancer IS reported State.Code == 'active'. The ALB data plane therefore reports a healthy forward target it cannot actually proxy to (either the task's container is not listening behind the ip target the emulator registered, or the forward action never connects and the emulator substitutes its own 502).

Why: a client relying on AWS's behavior can never get a response from its own service through the load balancer. No request succeeds, so nothing downstream of the data path exists either: no AWS/ApplicationELB HTTPCode_Target_2XX_Count, no target 5XX counts, and no access-log records describing real client requests — which is exactly the material an operator (or this check's diagnosis phase) needs to diagnose ALB 5xx. Worse, the health signal and the data path disagree: DescribeTargetHealth says healthy while every request 502s, so monitoring built on target health reports a working service that serves nothing.

Fix: the ELBv2 data plane's forward action — the path that accepts a viewer request at the published load-balancer address, matches the listener rule, and proxies to the registered ip target's address:port — together with whatever makes that target reachable (the ECS task's container endpoint the ip target names). The health evaluation must not report `healthy` for a target the forward path cannot reach, and a reachable target's own response must be relayed rather than replaced by an ELB-generated 502.

Related open issues that do NOT cover this check (neither carries its replay key): 20261111T030957Z-elbv2-s3-the-alb-answers-http-502-for.md, 20261110T192713Z-cloudwatch-ec2-elbv2-an-alb-answers-http-502.md and 20261119T043009Z-elbv2-5-net-new-causes-the-alb-nlb.md report the same 502-for-a-healthy-target class on other estates; 20261119T065938Z-cloudwatch-elbv2-s3-a-target-5xx-count-alarm.md is a different check in this service group.

Traces: cloudwatch__elbv2__s3.e2e-diagnose-alb-five-xx-errors-lifecycle (phase 1/5, deploy).

## Evidence
Failing check: [mismatch] cloudwatch__elbv2__s3.e2e-diagnose-alb-five-xx-errors-lifecycle

Expected: in phase 1 (deploy), after DescribeTargetHealth reports exactly one target with TargetHealth.State == 'healthy' on Port 8080 and DescribeLoadBalancers reports State.Code == 'active', a GET /health through the load balancer carrying the X-Sim2real-Run header matched by the forward rule answers HTTP 200 (the container's "ok").

Actual: AssertionError: [phase 1/5 deploy] a routed GET /health through '<alb address>' answering 200 not met within 180s; last (502, '') — every poll, 5 s apart for 180 s, got status 502 with an empty body.

Estate (from the check's inlined template): AWS::ElasticLoadBalancingV2::LoadBalancer (application, internet-facing) + Listener on 80 whose default action is fixed-response 404 + ListenerRule priority 10 conditioning on the http-header X-Sim2real-Run and forwarding to AWS::ElasticLoadBalancingV2::TargetGroup (TargetType ip, Port 8080, Protocol HTTP, HealthCheckPath /health) + AWS::ECS::Service (FARGATE, DesiredCount 1) whose container answers 200 "ok" on /health, 200 on POST /jobInstance/updateStatus and 500 otherwise.

## Steps to Reproduce
1. Deploy the estate: an internet-facing application load balancer, an HTTP:80 listener with a fixed-response 404 default action, a priority-10 listener rule matching header X-Sim2real-Run and forwarding to an ip target group on port 8080 with HealthCheckPath /health, and a Fargate service (DesiredCount 1) whose container listens on 8080 and answers /health with 200 "ok".
2. Poll DescribeTargetHealth on the target group until the one target reports State 'healthy' on Port 8080 (this succeeds), and DescribeLoadBalancers until State.Code == 'active' (this succeeds).
3. Send GET /health to the load balancer's published address with the X-Sim2real-Run header the rule matches.
4. AWS answers 200 with the container's body "ok"; the emulator answers 502 with an empty body, on every retry for at least 180 s.
