Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__elbv2.alb-health-check-marks-ec2-instance-unhealthy
Issue-Key: autoscaling__ec2__elbv2.alb-health-check-marks-ec2-instance-unhealthy|run#15|AssertionError|assert elapsed >= min_s - 5, (
Issue-Commit: dbc160e5ba09bc1337b7dfc453c49db2296d6587

# elbv2 verdicts a newly registered target unhealthy on the first probe, before UnhealthyThresholdCount x HealthCheckIntervalSeconds has elapsed

> _Note: this issue was identified against the code base at commit `dbc160e5ba09bc1337b7dfc453c49db2296d6587`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTargetHealth

## Description
One net-new root cause in the elbv2 target-health evaluation plane (no open issue carries this check's replay key, and it is not blocked by a scaffold stub).

Cause 1 of 1: DescribeTargetHealth reports a registered target as unhealthy immediately after RegisterTargets, without the threshold's consecutive failed health checks having been possible.
AWS: Smithy (com.amazonaws.elasticloadbalancingv2): CreateTargetGroup.UnhealthyThresholdCount (HealthCheckThresholdCount, range 2-10) is documented as 'The number of consecutive health check failures required before considering a target unhealthy', and HealthCheckIntervalSeconds (range 5-300) is the spacing between those checks. TargetHealth.State is TargetHealthStateEnum {initial, healthy, unhealthy, unhealthy.draining, unused, draining, unavailable} and TargetHealth.Reason documents, for state 'initial', 'Elb.RegistrationInProgress - the target is in the process of being registered with the load balancer' and 'Elb.InitialHealthChecking - the load balancer is still sending the target the minimum number of health checks required to determine its health status'; 'Target.ResponseCodeMismatch' is an 'unhealthy' reason meaning the health checks did not return an expected HTTP code. The requirement therefore follows from the model and the documented rule, not from any one recording: a target registered into a forwarded-to target group must remain in 'initial' (with the registration/initial-health-checking reason codes) until UnhealthyThresholdCount consecutive probes, spaced HealthCheckIntervalSeconds apart, have failed, i.e. not earlier than UnhealthyThresholdCount x HealthCheckIntervalSeconds after RegisterTargets, before DescribeTargetHealth may report State 'unhealthy' / Reason 'Target.ResponseCodeMismatch'.
Emulator: with HealthCheckIntervalSeconds=10, HealthyThresholdCount=2 and UnhealthyThresholdCount=2 (2 x 10 = 20 s minimum), DescribeTargetHealth already reports State 'unhealthy' with Reason 'Target.ResponseCodeMismatch' at the FIRST poll, 10.0 s after RegisterTargets — one interval in, so at most one probe can have failed. The verdict is produced from the configuration (health path that would 404) rather than from counted consecutive probe results, and the modeled 'initial' states with Elb.RegistrationInProgress / Elb.InitialHealthChecking are skipped.
Why: the state and reason are right but the timing is not, so anything that depends on the pre-verdict window gets the wrong answer: clients that gate a cutover on 'initial'/Elb.InitialHealthChecking while the minimum checks run, deployment tooling that measures time-to-unhealthy or counts flapping against the configured threshold, and tests asserting a target is NOT yet unhealthy within one interval all diverge. It also means the emulator's health status is a static function of the configuration, so a target whose server recovers between probes is never re-evaluated against HealthyThresholdCount either.
Fix: the target group's health-evaluation state behind DescribeTargetHealth — per-target consecutive pass/fail counters advanced on a HealthCheckIntervalSeconds schedule, holding State 'initial' with Reason 'Elb.RegistrationInProgress' then 'Elb.InitialHealthChecking' until UnhealthyThresholdCount (resp. HealthyThresholdCount) consecutive results exist, and only then projecting 'unhealthy'/'Target.ResponseCodeMismatch' (or 'healthy') in the DescribeTargetHealth serializer.
Traces: autoscaling__ec2__elbv2.alb-health-check-marks-ec2-instance-unhealthy.

Related but NON-COVERING open issues (other clusters of the elbv2 target-health surface): 20261109T205846Z-elasticloadbalancing-elbv2-responses-omit-modeled-target-health-fields-advertise.md, 20261118T090000Z-elbv2-target-health-on-silence-alpn-poll-ssl-policy-and-dnsname-port-addressing-remain-open.md.

## Evidence
Behavioral check autoscaling__ec2__elbv2.alb-health-check-marks-ec2-instance-unhealthy [mismatch]:

  AssertionError: target marked unhealthy after 10.0s, but unhealthy_threshold=2 x interval=10s requires >= 20s of real health checking

Topology the check builds: RunInstances (AL2023, httpd serving '/' only) -> CreateTargetGroup(Protocol=HTTP, Port=80, TargetType=instance, HealthCheckPath='/sim2real-missing-path', HealthCheckIntervalSeconds=10, HealthyThresholdCount=2, UnhealthyThresholdCount=2) -> CreateLoadBalancer(application, internet-facing) -> CreateListener(HTTP:80 forward to the target group) -> RegisterTargets(Targets=[{Id: <instance-id>, Port: 80}]), then DescribeTargetHealth polled every 10 s.

Expected (AWS): State stays 'initial' (Reason 'Elb.RegistrationInProgress', then 'Elb.InitialHealthChecking') for the first UnhealthyThresholdCount x HealthCheckIntervalSeconds = 20 s; only after that does State become 'unhealthy' with Reason 'Target.ResponseCodeMismatch'.
Actual (emulator): the first poll, 10.0 s after RegisterTargets, already returns TargetHealth{State: 'unhealthy', Reason: 'Target.ResponseCodeMismatch'} — the threshold's consecutive failures cannot have occurred in that window.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay behavioral check autoscaling__ec2__elbv2.alb-health-check-marks-ec2-instance-unhealthy (suite simparity-behavioral-aws-v1).
3. Or by hand against the emulator: RunInstances one instance in a subnet with an SG allowing tcp/80; CreateTargetGroup(Protocol=HTTP, Port=80, VpcId=<vpc>, TargetType=instance, HealthCheckProtocol=HTTP, HealthCheckPath='/does-not-exist', HealthCheckIntervalSeconds=10, HealthyThresholdCount=2, UnhealthyThresholdCount=2); CreateLoadBalancer(Type=application) and CreateListener(HTTP:80) forwarding to that target group; RegisterTargets with the instance on port 80 and record the wall-clock time.
4. Call DescribeTargetHealth repeatedly and record the elapsed time at which TargetHealth.State first leaves 'initial'.
5. AWS does not report 'unhealthy' before UnhealthyThresholdCount x HealthCheckIntervalSeconds (20 s here) and reports Reason 'Elb.RegistrationInProgress'/'Elb.InitialHealthChecking' until then; the emulator reports 'unhealthy'/'Target.ResponseCodeMismatch' within the first interval (10 s).
