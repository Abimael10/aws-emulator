Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__sns.composite-alarm-notifies-sns
Issue-Key: cloudwatch__sns.composite-alarm-notifies-sns|_poll_until#13|Timeout|time.sleep(interval_s)
Issue-Commit: 73ee5db70c54544c27a351cd5968d4682ba0e095

# cloudwatch__sns: a composite alarm whose child is forced to ALARM never transitions, so its SNS AlarmAction publishes no composite notification (no AlarmRule / TriggeringChildren payload)

> _Note: this issue was identified against the code base at commit `73ee5db70c54544c27a351cd5968d4682ba0e095`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutCompositeAlarm

## Description
ONE net-new root cause. The failing check below was not marked ALREADY FILED or OWNED by the replay report. ONE cause must be closed before submitting.

Prior work that does NOT carry this failure: 20261110T231031Z-cloudwatch-sns-sqs-a-composite-alarm-with-an--r2.md is about ActionsSuppressedBy on a composite with an ActionsSuppressor (a different service heading and a different field); 20261114T170523Z-cloudwatch-sns-a-cfn-deployed-alarm-s-sns.md is about a CFN-deployed METRIC alarm's SNS topic policy; 20261108T001650Z-cloudwatch-3-net-new-causes-a-composite-alarmrule.md owns only the AND-combinator checks under the `cloudwatch` heading. The same underlying reclaim is reported for the `cloudwatch` heading in the companion issue filed in this sweep (composite alarms never observably reach ALARM from a child forced by SetAlarmState); this one is the SNS delivery face of it and carries its own replay key.

=== Cause 1 of 1: a composite alarm's ALARM transition — and therefore its SNS AlarmAction publish — never happens when the child is driven by SetAlarmState ===
Failing check id: cloudwatch__sns.composite-alarm-notifies-sns.
AWS: Smithy (services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json) — PutCompositeAlarm input AlarmRule @required (@length 1..10240) and AlarmActions (ResourceList, 0..5) "the actions to execute when this alarm transitions to the ALARM state"; the operation documents that composite alarms "can take the following actions: Notify Amazon SNS topics" and that after creation "the alarm is then evaluated and its state is set appropriately. Any actions associated with the new state are then executed." SetAlarmState documents "When the updated state differs from the previous value, the action configured for the appropriate state is invoked" and that a composite returns to its actual state "only once any of its children alarms change state". Recorded behavior: forcing the referenced child to ALARM makes the composite transition to ALARM within seconds, and the composite's AlarmActions publish ONE notification to the SNS topic whose Message JSON carries AlarmName=<composite>, NewStateValue='ALARM', the composite's AlarmRule verbatim, and a TriggeringChildren list naming the child alarm that made the rule true.
Emulator: the composite is created with AlarmActions=[topic] and is visible via DescribeAlarms(AlarmTypes=['CompositeAlarm']) with a non-ALARM baseline, but after SetAlarmState(child,'ALARM') the composite is never read in ALARM and no message with AlarmName=<composite> ever arrives on the SNS->SQS path; the check exhausts its budget (`Check timed out after 60.0s`, polling windows of 90 s for the state and 90 s for the message). The positive control in the same check — topic, SQS subscription and the cloudwatch.amazonaws.com publish policy — is set up successfully, so the absent notification follows from the absent transition. Hypothesis, grounded in the checkout: the forced child state is reclaimed after at most 3 s (FORCED_STATE_SECS, services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/state/alarm_evaluation.rs:357, forced_state_span_secs ~line 921) while the parent's evaluation is owed COMPOSITE_EVALUATION_DELAY_SECS later (src/state.rs:2942, make_parents_due src/state.rs:3238), so the rule is evaluated against a child already out of ALARM and no composite transition — hence no action — is ever enqueued.
Why: the whole point of a composite alarm is to be the single thing that pages. A client that wires a composite's AlarmActions to SNS and tests it against the emulator receives nothing, so an alerting topology that would page in production looks silent here, and the absent notification is indistinguishable from a correctly suppressed one.
Fix: the composite-alarm transition path in the alarm state engine — a forced child state must stand until that child's own next evaluation and be the state the composite pass evaluates, and a composite transition into ALARM must enqueue its AlarmActions notification whose payload carries NewStateValue, the AlarmRule as stored, and TriggeringChildren naming the children that made the rule true.
Traces: cloudwatch__sns.composite-alarm-notifies-sns.

## Evidence
Behavioral suite simparity-behavioral-aws-v1: 909/1226 passed. Service heading `### cloudwatch__sns` — 14/16 (88%), 2 failed; the check below is the one not marked ALREADY FILED or OWNED (the other, cloudwatch__sns.cfn-alarm-missing-sns-topic-policy-no-delivery, is OWNED by 20261114T170523Z-cloudwatch-sns-a-cfn-deployed-alarm-s-sns.md).

[mismatch] cloudwatch__sns.composite-alarm-notifies-sns — `Check timed out after 60.0s`. Setup that succeeded: sns.create_topic; sqs.create_queue + set_queue_attributes with an sqs:SendMessage policy for sns.amazonaws.com; sns.subscribe(Protocol='sqs'); sns.set_topic_attributes(Policy allowing SNS:Publish to cloudwatch.amazonaws.com); put_metric_alarm(child, Namespace='sim2real/cwsns', Period=60, EvaluationPeriods=1, Threshold=1.0, GreaterThanThreshold, TreatMissingData='notBreaching'); put_composite_alarm(composite, AlarmRule='ALARM("<child>")', AlarmActions=[topic_arn]). Then set_alarm_state(child,'ALARM') and the check polls describe_alarms(AlarmNames=[composite], AlarmTypes=['CompositeAlarm']) every 3 s for 90 s for StateValue=='ALARM' and sqs.receive_message for a Message with AlarmName==<composite>; neither is ever seen.

Expected notification shape (requirement, not one run's values): the SNS Message JSON for the composite's ALARM transition carries AlarmName = the composite's name, NewStateValue = 'ALARM', AlarmRule = the rule exactly as stored by PutCompositeAlarm, and TriggeringChildren listing the child alarm(s) whose state made the rule true. Generated topic/queue/alarm names, ARNs, timestamps and message ids are per-run values and are not requirements.

## Steps to Reproduce
Point boto3 cloudwatch/sns/sqs clients at the emulator.
1. create_topic; create_queue; set_queue_attributes(Policy allowing sns.amazonaws.com sqs:SendMessage); subscribe(Protocol='sqs', Endpoint=<queue arn>); set_topic_attributes(AttributeName='Policy', AttributeValue=<allow SNS:Publish to cloudwatch.amazonaws.com>).
2. put_metric_alarm(AlarmName='child', Namespace='<fresh>', MetricName='<fresh>', Statistic='Average', Period=60, EvaluationPeriods=1, Threshold=1.0, ComparisonOperator='GreaterThanThreshold', TreatMissingData='notBreaching').
3. put_composite_alarm(AlarmName='comp', AlarmRule='ALARM("child")', AlarmActions=[<topic arn>]).
4. set_alarm_state(AlarmName='child', StateValue='ALARM', StateReason='test').
5. Poll describe_alarms(AlarmNames=['comp'], AlarmTypes=['CompositeAlarm']). Expected: StateValue 'ALARM' within seconds. Observed: never.
6. Poll receive_message on the queue. Expected: one SNS envelope whose Message JSON has AlarmName='comp', NewStateValue='ALARM', AlarmRule='ALARM("child")' and TriggeringChildren naming 'child'. Observed: no message for the composite at all.
7. Re-replay to confirm the fix: cloudwatch__sns.composite-alarm-notifies-sns.
