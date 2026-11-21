Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2__elbv2.albos-cli-solve-queries-find-the-alb-by-dns-name-and-its-stale-targets
Issue-Key: cloudwatch__ec2__elbv2.albos-cli-solve-queries-find-the-alb-by-dns-name-and-its-stale-targets|run#22|AssertionError|assert state == ["ALARM"], f"the breaching unhealthy-hosts alarm should read ALARM, got {state!r}"
Issue-Commit: 7ce5552927764f5e5adc0c494ca5a7d589c732e6

# cloudwatch__ec2__elbv2: an AWS/ApplicationELB UnHealthyHostCount alarm with TreatMissingData=breaching settles OK while the target group holds only unhealthy targets

> _Note: this issue was identified against the code base at commit `7ce5552927764f5e5adc0c494ca5a7d589c732e6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAlarms

## Description
One net-new root cause.

Cause 1 of 1: a metric alarm over AWS/ApplicationELB UnHealthyHostCount, dimensioned on the ALB and its target group and created with TreatMissingData='breaching', leaves INSUFFICIENT_DATA into OK instead of ALARM.
AWS: recorded behavior plus the documented alarm-evaluation rule (https://docs.aws.amazon.com/AmazonCloudWatch/latest/monitoring/AlarmThatSendsEmail.html). The alarm is PutMetricAlarm(Namespace='AWS/ApplicationELB', MetricName='UnHealthyHostCount', Dimensions=[TargetGroup=<targetgroup/.../...>, LoadBalancer=<app/.../...>], Statistic='Maximum', Period=60, EvaluationPeriods=2, Threshold=0, ComparisonOperator='GreaterThanThreshold', TreatMissingData='breaching'). Both evaluation inputs point the same way on AWS: an internal ALB whose listener forwards to a target group holding two registered IP targets that answer no health check publishes UnHealthyHostCount=2 for that TargetGroup+LoadBalancer pair, which is > 0 for two periods; and where no datapoint exists at all, TreatMissingData='breaching' makes the missing period count as breaching. Either way DescribeAlarms must report MetricAlarms[0].StateValue = 'ALARM' once the alarm leaves INSUFFICIENT_DATA.
Emulator: the alarm does leave INSUFFICIENT_DATA (so the check's earlier reads — the ALB found by DNSName, its VPC, its two target groups, their registered stale targets, and the ALB's own ENIs in the VPC — all pass), and then reports StateValue 'OK' and keeps reporting it for the whole 600 s budget. Nothing ever drives the alarm breaching: the AWS/ApplicationELB UnHealthyHostCount series for the group is treated as non-breaching rather than as 2 unhealthy hosts, and a period with no datapoint is not graded against TreatMissingData='breaching'.
Why: this is the exact signal an operator alarms on to learn that a target group is serving nothing. A client (console query, CLI --query trajectory, runbook or autoscaling/notification wiring keyed on the alarm state) reads OK from a load balancer with zero healthy targets, so the failure is invisible and no alarm action is ever dispatched. TreatMissingData='breaching' being ignored also inverts every 'no data means broken' alarm, not just this one.
Fix: CloudWatch's metric-alarm evaluation for this path — the ELBv2 publisher of AWS/ApplicationELB UnHealthyHostCount per TargetGroup+LoadBalancer dimension pair, and the alarm evaluator's handling of a missing datapoint under TreatMissingData (it must grade the period as breaching, not as OK).
Traces: cloudwatch__ec2__elbv2.albos-cli-solve-queries-find-the-alb-by-dns-name-and-its-stale-targets.

## Evidence
cloudwatch__ec2__elbv2.albos-cli-solve-queries-find-the-alb-by-dns-name-and-its-stale-targets (mismatch):
  AssertionError: the breaching unhealthy-hosts alarm should read ALARM, got ['OK']
  Expected: `aws cloudwatch describe-alarms --alarm-names <alarm> --query MetricAlarms[0].StateValue` -> ALARM (UnHealthyHostCount Maximum = 2 > Threshold 0 for 2 periods of 60 s; and/or missing datapoints graded breaching via TreatMissingData='breaching').
  Actual: OK, stable for the check's 600 s ceiling. The preceding assertions in the same check pass, so the ALB, its target groups, their registered targets and the ALB's ENIs are all present — only the alarm verdict diverges.

## Steps to Reproduce
Replay cloudwatch__ec2__elbv2.albos-cli-solve-queries-find-the-alb-by-dns-name-and-its-stale-targets (behavioral suite 'simparity-behavioral-aws-v1'), or by hand:
1. Create a VPC with three /22 subnets across two AZs and a security group.
2. Create three target groups (TargetType ip, HealthCheckProtocol HTTPS, HealthCheckPort 443, interval 30 s, timeout 10 s, HealthyThresholdCount 2, UnhealthyThresholdCount 3, Matcher 200,301,302) and RegisterTargets two in-CIDR addresses that are attached to nothing, on port 443, into each.
3. Create an internal ALB in two of the subnets; create an HTTP:80 listener whose default action forwards to the first group and a priority-10 path-pattern rule forwarding to the second.
4. PutMetricAlarm(Namespace='AWS/ApplicationELB', MetricName='UnHealthyHostCount', Dimensions=[{TargetGroup: <tg ARN suffix>}, {LoadBalancer: <lb ARN suffix>}], Statistic='Maximum', Period=60, EvaluationPeriods=2, Threshold=0, ComparisonOperator='GreaterThanThreshold', TreatMissingData='breaching').
5. Poll DescribeAlarms until StateValue != 'INSUFFICIENT_DATA' and read it: AWS reports ALARM, the emulator reports OK.
