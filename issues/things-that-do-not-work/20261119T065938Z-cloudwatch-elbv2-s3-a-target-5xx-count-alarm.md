Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__elbv2__s3.target-5xx-rate-alarm-stays-ok-under-a-low-error-rate-that-fires-a-count-alarm
Issue-Key: cloudwatch__elbv2__s3.target-5xx-rate-alarm-stays-ok-under-a-low-error-rate-that-fires-a-count-alarm|_call#17|Timeout|with urllib.request.urlopen(req, timeout=30) as resp:
Issue-Commit: 93aea946e1be9d371a5c94c0146ea55c9b1afa4f

# cloudwatch__elbv2__s3: a Target_5XX count alarm never fires for ALB traffic, so the metric-math 5xx-rate closed-loop check exhausts its 600 s budget

> _Note: this issue was identified against the code base at commit `93aea946e1be9d371a5c94c0146ea55c9b1afa4f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutMetricAlarm

## Description
One net-new root cause with one grounded check (Cause 1 of 1). It is in the same family as open issue 20261110T220525Z-cloudwatch-elbv2-s3-no-aws-applicationelb-metric-datapoint.md (no AWS/ApplicationELB datapoint ever lands for Lambda-target ALB traffic), but that filing does not carry this check's replay key and the suite did not mark this check OWNED, so it survives as a live failure; it adds the alarm-evaluation half of the behaviour, which that issue does not cover.

Cause 1 of 1: with a Lambda-target ALB serving a mix of 200s and 500s, neither a plain Sum alarm on AWS/ApplicationELB HTTPCode_Target_5XX_Count nor a metric-math alarm on 100 * FILL(m1,0) / m2 (Target_5XX over RequestCount) ever completes the evaluation the scenario waits for, and the check is killed by its 600 s ceiling.
AWS: Smithy (cloudwatch) PutMetricAlarm with Namespace AWS/ApplicationELB, MetricName HTTPCode_Target_5XX_Count, Dimensions LoadBalancer=app/<name>/<id> and TargetGroup=targetgroup/<name>/<id>, Statistic Sum, Period 60, EvaluationPeriods 1, DatapointsToAlarm 1, Threshold 0, ComparisonOperator GreaterThanThreshold, TreatMissingData notBreaching; and PutMetricAlarm with Metrics=[two MetricStat entries with ReturnData false plus an Expression "100 * FILL(m1, 0) / m2" with ReturnData true], Threshold 50. Recorded behavior: each request the load balancer answers is counted into the per-minute AWS/ApplicationELB series (RequestCount, and HTTPCode_Target_5XX_Count for a 5xx the target itself returned) under those dimensions; a period-60, 1-of-1 alarm then evaluates within a few minutes of the datapoint. The requirement is the closed loop: one 500 among nine 200s drives the COUNT alarm to ALARM while the RATE alarm completes an evaluation and stays OK (its history holding no ALARM at all, because the rate stays under 50), and once every request answers 500 BOTH alarms read ALARM. The particular counts come from the requests the check drives; the attribution and the threshold comparison are the requirement.
Emulator: the check never gets an answer to grade — "Error: Check timed out after 600.0s". The scenario's own ceilings are the count alarm reaching ALARM and the rate alarm leaving INSUFFICIENT_DATA, both 600 s; the run dies inside that window, so no alarm transition was observed. Hypothesis (the replay output carries only the timeout): the AWS/ApplicationELB RequestCount / HTTPCode_Target_5XX_Count series is never published for these requests — exactly the defect open issue 20261110T220525Z-... records for the sibling checks under this heading — so both alarms sit on empty series, the count alarm never leaves notBreaching-OK and the rate alarm never completes an evaluation.
Why: a client's only automated signal that a Lambda-backed ALB's targets are erroring is an alarm on these series. Against the emulator a count alarm on target 5xx never fires and a metric-math rate alarm never evaluates, so alarm wiring, SNS/scaling actions hung off it, and the very distinction a rate threshold exists to draw (a low error rate must NOT page, a total outage must) cannot be exercised at all — and code validated here will not page on real AWS either, because the behaviour was never actually observed.
Fix: the AWS/ApplicationELB metric publisher for load-balancer-served requests (per-request RequestCount and HTTPCode_Target_5XX_Count under the LoadBalancer and TargetGroup dimensions, target-vs-ELB attribution) together with the CloudWatch alarm evaluator for both plain-metric and Metrics/Expression (metric-math, FILL) alarms, so a 1-of-1 period-60 alarm transitions on the datapoints those requests produce.
Traces: cloudwatch__elbv2__s3.target-5xx-rate-alarm-stays-ok-under-a-low-error-rate-that-fires-a-count-alarm.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator; service heading `### cloudwatch__elbv2__s3` 12/18 (67%), 6 failed — 5 marked OWNED, the id in trace_ids survives.

[mismatch] cloudwatch__elbv2__s3.target-5xx-rate-alarm-stays-ok-under-a-low-error-rate-that-fires-a-count-alarm — Error: Check timed out after 600.0s

The check's setup: a python3.12 Lambda answering 200 on /health and on POST /jobInstance/updateStatus and 500 on any other route, registered as the only target of a lambda-type target group behind an internet-facing ALB whose listener :80 forwards only requests carrying this run's X-Sim2real-Run header (default action a fixed 404). Two alarms: `<tok>-target-5xx-count` (AWS/ApplicationELB HTTPCode_Target_5XX_Count, Sum, Period 60, 1-of-1, Threshold 0, GreaterThanThreshold, TreatMissingData notBreaching, Dimensions LoadBalancer + TargetGroup) and `<tok>-target-5xx-rate` (Metrics: m1 HTTPCode_Target_5XX_Count, m2 RequestCount, both ReturnData false; expression "100 * FILL(m1, 0) / m2" ReturnData true; Threshold 50, GreaterThanThreshold, 1-of-1).

Expected (AWS): driving five 200s, one 500, four 200s repeatedly brings the COUNT alarm to ALARM within _ALARM_DEADLINE_S (600 s); the RATE alarm completes an evaluation and reads OK, with DescribeAlarmHistory StateUpdate items containing no "ALARM"; then driving six 500s brings BOTH to ALARM.
Actual (emulator): the check is killed at its 600 s ceiling with no alarm transition observed — no state is graded at all.

Sibling checks under the same heading fail the same way and are owned by 20261110T220525Z-cloudwatch-elbv2-s3-no-aws-applicationelb-metric-datapoint.md ("Check timed out after 600.0s", no AWS/ApplicationELB datapoint within the ingestion window); this check's replay key is not in that issue.

Smithy consulted: services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/... — PutMetricAlarm (Metrics/MetricDataQuery with Expression and ReturnData, Threshold/ComparisonOperator/TreatMissingData), DescribeAlarms, DescribeAlarmHistory. Metric names and the target-vs-ELB 5xx attribution: recorded behavior plus the ALB CloudWatch-metrics documentation the check cites.

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; cloudwatch__elbv2__s3.target-5xx-rate-alarm-stays-ok-under-a-low-error-rate-that-fires-a-count-alarm reproduces (times out at 600 s).
2. Create a Lambda function whose handler returns 200 for /health and for POST /jobInstance/updateStatus and 500 for any other path; grant elasticloadbalancing.amazonaws.com lambda:InvokeFunction with SourceArn the target group.
3. Create a lambda-type target group with a /health health check, RegisterTargets the function, create an internet-facing ALB with an HTTP :80 listener (default fixed-response 404) and one rule forwarding requests with a run header to the target group; wait for the load balancer to be active and a routed GET /health to answer 200.
4. PutMetricAlarm the count alarm: Namespace AWS/ApplicationELB, MetricName HTTPCode_Target_5XX_Count, Dimensions LoadBalancer + TargetGroup, Statistic Sum, Period 60, EvaluationPeriods 1, DatapointsToAlarm 1, Threshold 0, GreaterThanThreshold, TreatMissingData notBreaching.
5. PutMetricAlarm the rate alarm: Metrics = [m1 HTTPCode_Target_5XX_Count (ReturnData false), m2 RequestCount (ReturnData false), rate = "100 * FILL(m1, 0) / m2" (ReturnData true)], Threshold 50, GreaterThanThreshold, 1-of-1.
6. Repeatedly POST five 200-answering paths, one 500-answering path, four 200-answering paths through the load balancer. Expected: DescribeAlarms shows the count alarm ALARM and the rate alarm OK (and DescribeAlarmHistory for the rate alarm holds no ALARM state). Actual: nothing transitions inside 600 s.
7. Then POST only 500-answering paths. Expected: both alarms ALARM.
8. Also read AWS/ApplicationELB RequestCount and HTTPCode_Target_5XX_Count for those dimensions to confirm whether any datapoint lands at all (see 20261110T220525Z-cloudwatch-elbv2-s3-no-aws-applicationelb-metric-datapoint.md).
9. Re-replay the id in trace_ids to confirm the fix.
