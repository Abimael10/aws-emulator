Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elbv2__s3.log-edge-producers-alb-access-log-502-record-differs-from-target-status
Issue-Key: elbv2__s3.log-edge-producers-alb-access-log-502-record-differs-from-target-status|_poll_until#30|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: 8465be9c60d4839857b29976b5cf2d08632066fc

# elbv2__s3: a registered instance target never reaches TargetHealth.State 'healthy', so the ALB access-log record for a 502 can never be produced or graded

> _Note: this issue was identified against the code base at commit `8465be9c60d4839857b29976b5cf2d08632066fc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTargetHealth

## Description
One net-new root cause. The failing check is not marked OWNED or ALREADY FILED by the replay report. The nearest open issue is 20261109T153103Z-elbv2-4-net-new-causes-the-published-dnsname.md (published DNSName not served; target health causes); it does not carry this check's replay key, so this is filed separately and names it for context.

Cause 1 of 1 — DescribeTargetHealth never reports 'healthy' for an instance target registered in an HTTP target group with a fast health check, so every downstream data-plane and access-log assertion is unreachable.
AWS: Smithy (elasticloadbalancingv2): DescribeTargetHealth returns TargetHealthDescriptions[].TargetHealth{State,Reason,Description}; TargetHealthStateEnum = initial|healthy|unhealthy|unhealthy.draining|unused|draining|unavailable; the Reason doc states the 'initial' reasons are Elb.RegistrationInProgress / Elb.InitialHealthChecking and that when the state is 'healthy' no Reason and no Description is provided. For a target group with HealthCheckProtocol=HTTP, HealthCheckPath=/health, HealthCheckIntervalSeconds=5 and HealthyThresholdCount=2, a target whose health endpoint answers 200 must leave 'initial' and read State='healthy' after roughly two intervals — a bounded, short wait, not an unbounded one.
Emulator: the polled state never becomes 'healthy'. The check polls DescribeTargetHealth(TargetGroupArn, Targets=[{Id:'i-...', Port:8081}]) every 2 s against a 600 s cap and dies with: TimeoutError: "target {'Id': 'i-00000000000000001', 'Port': 8081} healthy (observed []): the run budget ended first; not graded" (the parenthesised list is formatted before the poll begins, so it carries no observed states — the load-bearing fact is that 'healthy' is never returned). Because the wait never completes, the behavior the check exists to grade — the ALB access-log line for a request the ALB answers 502 while the target reports its own status, i.e. elb_status_code vs target_status_code in the documented field order delivered to S3 — is never exercised at all.
Why: a client that gates traffic, a deployment, or log verification on a target becoming healthy hangs indefinitely; and because no request is ever served through the listener, no access-log record is written to the S3 bucket, so the whole access-log contract (field count, order and the elb_status_code/target_status_code split) is unobservable against the emulator.
Fix: ELBv2 target-health evaluation — the health-check state machine behind DescribeTargetHealth must probe the registered instance target's health path and advance initial (Elb.InitialHealthChecking) -> healthy (Reason/Description absent) within the configured interval x HealthyThresholdCount, so the listener will route and the access-log writer will emit a record.
Traces: elbv2__s3.log-edge-producers-alb-access-log-502-record-differs-from-target-status.

## Evidence
Expected (AWS): after RegisterTargets of instance target {Id: i-..., Port: 8081} in a target group with HealthCheckProtocol=HTTP, HealthCheckPath=/health, HealthCheckIntervalSeconds=5, HealthCheckTimeoutSeconds=2, HealthyThresholdCount=2, DescribeTargetHealth reports TargetHealth.State 'initial' (Reason Elb.RegistrationInProgress or Elb.InitialHealthChecking) and then 'healthy' with Reason and Description absent, within about two health-check intervals.

Actual (emulator): the state never becomes 'healthy'. Replay output: File "check_elbv2__s3_log_edge_producers_alb_access_log_502_record_differs_from_target_status.py", line 295, in _wait_healthy ... raise TimeoutError(f"{description}: the run budget ended first; not graded") -> TimeoutError: target {'Id': 'i-00000000000000001', 'Port': 8081} healthy (observed []): the run budget ended first; not graded. Consequently no request is served through the listener and no ALB access-log record reaches the S3 bucket. Model grounding: TargetHealthStateEnum = [initial, healthy, unhealthy, unhealthy.draining, unused, draining, unavailable]; TargetHealth.Reason doc: 'If the target state is initial, the reason code can be ... Elb.RegistrationInProgress ... Elb.InitialHealthChecking'; 'If the target state is healthy, a reason code is not provided.'

## Steps to Reproduce
1. Create a VPC/subnet and an instance serving HTTP on the traffic port and answering 200 on /health on the health-check port.
2. CreateTargetGroup(Protocol=HTTP, Port=<traffic port>, VpcId=<vpc>, TargetType=instance, HealthCheckProtocol=HTTP, HealthCheckPort=<health port>, HealthCheckPath=/health, HealthCheckIntervalSeconds=5, HealthCheckTimeoutSeconds=2, HealthyThresholdCount=2).
3. Create an internet-facing ALB with an HTTP:80 listener forwarding to that target group, and enable access logs to an S3 bucket granting logdelivery.elasticloadbalancing.amazonaws.com.
4. RegisterTargets({Id: <instance id>, Port: <traffic port>}).
5. Poll DescribeTargetHealth(TargetGroupArn, Targets=[that target]) every 2 s -> expect State 'healthy' with no Reason within ~10 s; observed: 'healthy' is never returned and the poll exhausts its budget.
6. (Unreachable in the emulator) Send a request the ALB answers 502 and read the delivered access-log line: expect one record in the documented field order whose elb_status_code is 502 and whose target_status_code reflects the target's own answer.
