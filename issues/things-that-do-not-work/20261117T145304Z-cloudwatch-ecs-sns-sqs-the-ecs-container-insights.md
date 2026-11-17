Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__sns__sqs.obs-ecs-1-alarm-notification-withheld-from-queue-without-sns-policy
Issue-Key: cloudwatch__ecs__sns__sqs.obs-ecs-1-alarm-notification-withheld-from-queue-without-sns-policy|_poll_until#26|Timeout|time.sleep(delay)
Issue-Commit: 6df92bd208b59385c9370bd54f69012d5d2b8998

# cloudwatch__ecs__sns__sqs: the ECS Container-Insights alarm fan-out scenario never reaches an observable step — the withheld-notification check is killed at its 60 s cap with no verdict

> _Note: this issue was identified against the code base at commit `6df92bd208b59385c9370bd54f69012d5d2b8998`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutMetricAlarm

## Description
ONE net-new root cause, grounded in the only failing check of the suite's `cloudwatch__ecs__sns__sqs` heading that the replay report marks neither ALREADY FILED nor OWNED.

Cause 1 of 1: an alarm driven by an ECS Container-Insights metric never produces the observable ALARM->SNS->SQS hand-off the scenario is built on, so the check cannot reach any assertion.
Failing check id: cloudwatch__ecs__sns__sqs.obs-ecs-1-alarm-notification-withheld-from-queue-without-sns-policy

AWS: a Container-Insights-enabled ECS cluster running a service with desiredCount 0 reports an ECS/ContainerInsights RunningTaskCount below 1 (or no datapoint, which the alarm's TreatMissingData setting treats as breaching), so a metric alarm whose AlarmActions name an SNS topic settles in ALARM on its own and SNS publishes one notification per transition. SNS then delivers that notification to each subscribed SQS queue whose queue policy grants sqs:SendMessage to the topic, and delivers NOTHING to a subscribed queue that lacks the grant — the granted sibling receives every round while the ungranted one stays empty, and adding the grant makes the previously silent queue receive. Recorded behavior only (no model text covers alarm evaluation or SNS->SQS authorization).

Emulator: the check is killed at the suite's 60 s cap with no verdict — it never gets far enough to assert either the delivery to the granted queue or the absence of delivery to the ungranted one. Hypothesis: the chain does not advance — either ECS/ContainerInsights RunningTaskCount is never published for the service so the alarm never leaves INSUFFICIENT_DATA, or the alarm's ALARM transition never hands off to SNS. The emulator serves every call the check makes, so the stall is the emulator's: no step of the scenario produced its effect inside the budget.

Why: the alarm->SNS->SQS path is the standard CloudWatch notification fan-out. If it never produces a notification, every client that provisions alarms with SNS actions observes silence for a condition AWS alarms on, and the negative half (an unauthorized queue must receive nothing) is untestable because nothing is delivered to any queue — a missing grant and a broken pipeline are indistinguishable.

Fix: the CloudWatch alarm evaluator and its action hand-off in the cloudwatch service crate (the component that moves a metric alarm to ALARM and publishes to each AlarmActions SNS topic), plus the ECS Container-Insights metric publication that feeds RunningTaskCount into the CloudWatch metric store. Related OPEN issues on the alarm->SNS hand-off, named for context only and NOT covering this check (the report marks it neither ALREADY FILED nor OWNED): 20261114T170523Z-cloudwatch-sns-a-cfn-deployed-alarm-s-sns.md, 20261116T060651Z-cloudwatch-sns-a-composite-alarm-whose-child-is.md.

Traces: cloudwatch__ecs__sns__sqs.obs-ecs-1-alarm-notification-withheld-from-queue-without-sns-policy.

## Evidence
Failing check (suite service heading `### cloudwatch__ecs__sns__sqs`): cloudwatch__ecs__sns__sqs.obs-ecs-1-alarm-notification-withheld-from-queue-without-sns-policy
  Error: Check timed out after 60.0s — no assertion was reached.

What the check sets up before the stall: ecs:CreateCluster with containerInsights=enabled; ecs:RegisterTaskDefinition; an ECS service with zero tasks; a CloudWatch metric alarm on ECS/ContainerInsights RunningTaskCount that treats missing data as breaching; an SNS topic as AlarmActions; two SQS queues subscribed to the topic, one WITH a queue policy granting sqs:SendMessage to the topic and one WITHOUT.
Expected (AWS): the alarm reaches ALARM without intervention; the granted queue receives the notification on every round (_EXTRA_GOOD_ROUNDS additional rounds included); the ungranted queue receives nothing; after the grant is added it receives.
Actual (emulator): the check never reaches any of those observations within the 60 s cap, i.e. the alarm/notification chain produced no observable effect.

## Steps to Reproduce
1. ecs:CreateCluster(settings=[{name: containerInsights, value: enabled}]); ecs:RegisterTaskDefinition (EC2/bridge, one container); create a service in that cluster with desiredCount 0.
2. sns:CreateTopic; sqs:CreateQueue twice; sns:Subscribe both queues (protocol sqs); set a Policy on ONE queue allowing sqs:SendMessage for Principal Service sns.amazonaws.com with aws:SourceArn = the topic; leave the other queue with no policy.
3. cloudwatch:PutMetricAlarm on ECS/ContainerInsights RunningTaskCount for that cluster/service, ComparisonOperator LessThanThreshold, Threshold 1, TreatMissingData breaching, AlarmActions=[<topic arn>].
4. Poll cloudwatch:DescribeAlarms for StateValue ALARM and sqs:ReceiveMessage on both queues. EXPECT ALARM, one notification per transition on the granted queue, nothing on the ungranted one; then add the grant and EXPECT the next round to arrive. OBSERVE no alarm transition / no delivery within the budget (the check is killed at 60 s).
Replay: cloudwatch__ecs__sns__sqs.obs-ecs-1-alarm-notification-withheld-from-queue-without-sns-policy
