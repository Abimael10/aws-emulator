Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__events__sqs.xsvc-cloudwatch-2-alarm-configuration-change-events-route-create-update-delete-only-to-the-matching-rule cloudwatch__events__sqs.xsvc-cloudwatch-2-state-changes-tagging-and-noop-puts-emit-no-configuration-change-event cloudwatch__events__sqs.xsvc-cloudwatch-1-alarm-state-change-chain-delivered-to-sqs
Issue-Key: cloudwatch__events__sqs.xsvc-cloudwatch-2-alarm-configuration-change-events-route-create-update-delete-only-to-the-matching-rule|_metric_of#39|AssertionError|assert len(metrics) == 1, (
Issue-Key: cloudwatch__events__sqs.xsvc-cloudwatch-2-state-changes-tagging-and-noop-puts-emit-no-configuration-change-event|_drain#25|Timeout|resp = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10, WaitTimeSeconds=2)
Issue-Key: cloudwatch__events__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Commit: 4bf99a6da981b1c03b4c2a4da03064374e7befb5

# cloudwatch__events__sqs: the CloudWatch Alarm Configuration Change event's configuration omits the metrics array, the emit-scope check never completes, and SendMessage's NonExistentQueue message carries the legacy wsdl clause (3 causes)

> _Note: this issue was identified against the code base at commit `4bf99a6da981b1c03b4c2a4da03064374e7befb5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutMetricAlarm

## Description
This issue bundles 3 independent net-new root causes in the cloudwatch__events__sqs path. Every cause is independent; all 3 must be closed before submitting.

Cause 1 of 3: the "CloudWatch Alarm Configuration Change" event's detail.configuration describes the alarm's metric with flat legacy keys and carries no `metrics` array at all.
AWS: recorded behavior only (no Smithy trait describes the EventBridge payload; PutMetricAlarm models Namespace, MetricName, Statistic, Period, Dimensions and the Metrics list as plain members). CloudWatch emits an EventBridge event with source "aws.cloudwatch" and detail-type "CloudWatch Alarm Configuration Change" whose detail.configuration describes the alarm in the same shape DescribeAlarms' metric-math form uses: a `metrics` array with exactly one entry for a single-metric alarm, that entry carrying `metricStat.metric.namespace`, `metricStat.metric.name` (and its dimensions) plus `metricStat.period` and `metricStat.stat`. The namespace/name/stat a consumer reads come from inside that array, not from top-level members.
Emulator: detail.configuration is emitted in a flat form with no `metrics` key — observed keys are alarmName, alarmArn, actionsEnabled, okActions, alarmActions, insufficientDataActions, namespace, metricName, statistic, dimensions, period, evaluationPeriods, threshold, comparisonOperator, treatMissingData. `configuration.get("metrics")` is therefore empty, so the metric identity of the alarm is unreadable from the event.
Why: every EventBridge consumer that reacts to an alarm configuration change by reading which metric the alarm watches (routing by namespace, re-deriving a dashboard, auditing drift) finds no metric at all and either crashes or silently skips the event. An input transformer or event-pattern filter keyed on detail.configuration.metrics matches nothing.
Fix: the serializer that builds the detail.configuration object for the CloudWatch Alarm Configuration Change event in the alarm event-emission path — it must emit the metrics/metricStat/metric structure for single-metric alarms (alongside previousConfiguration, which has the same shape).
Traces: cloudwatch__events__sqs.xsvc-cloudwatch-2-alarm-configuration-change-events-route-create-update-delete-only-to-the-matching-rule.

Cause 2 of 3: the emit-scope check of the same event family never completes within its budget.
AWS: recorded behavior only. Of six alarm writes, only the create, DisableAlarmActions and EnableAlarmActions emit a "CloudWatch Alarm Configuration Change"; SetAlarmState emits only a "CloudWatch Alarm State Change", and TagResource and a no-op PutMetricAlarm that changes nothing emit neither. Against the recording the check warms the rule→queue path up (five consecutive PutMetricAlarm writes, each matched to its own delivered Configuration Change by threshold) and then proves the absences, all comfortably inside the suite's 60 s budget.
Emulator: the check does not finish — the replay reports "Check timed out after 60.0s" with no assertion reached, so none of the six emit-scope facts is ever observed. Hypothesis: the configuration-change emission is not delivered for every PutMetricAlarm write, so the warm-up's five-consecutive-deliveries streak keeps resetting and consumes the whole budget; the sibling check of Cause 1 does receive such events, so the path is not dead, only unreliable or slow.
Why: a consumer cannot rely on one configuration change per alarm write — a missed emission means an audit or drift-detection pipeline silently loses a change — and the whole emit-scope contract (which write emits which detail-type) is unverifiable.
Fix: the alarm event-emission path in the CloudWatch alarm write handlers (PutMetricAlarm/TagResource/Disable- and EnableAlarmActions/SetAlarmState) — each qualifying write must emit exactly one event of the right detail-type, promptly and for every write, including repeated writes to the same alarm.
Traces: cloudwatch__events__sqs.xsvc-cloudwatch-2-state-changes-tagging-and-noop-puts-emit-no-configuration-change-event.

Cause 3 of 3: SendMessage to a deleted queue carries the legacy query-protocol "for this wsdl version" clause.
AWS: Smithy: com.amazonaws.sqs#QueueDoesNotExist is a client error with httpError 400 and awsQueryError code AWS.SimpleQueueService.NonExistentQueue; the model fixes no message text. Recorded behavior: SendMessage against a deleted queue's URL is refused with HTTP 400, Error.Code "AWS.SimpleQueueService.NonExistentQueue" and Error.Message "The specified queue does not exist." — the complete fixed text, with no variable part (it deliberately does not name the queue). The "… for this wsdl version." wording belongs to the legacy AWS Query/XML SQS endpoint, not to the JSON protocol these clients speak.
Emulator: right status and code, message "The specified queue does not exist for this wsdl version." — a single hard-coded constant shared by every URL-addressed SQS lookup.
Why: a client that tells a deleted queue apart by the refusal message never matches, and the alarm-state-change delivery check aborts on its deleted-queue leg before asserting the chain it exists to prove.
Fix: the SQS queue-resolution refusal message constant shared by every URL-addressed operation — it must be "The specified queue does not exist." (the NAME-addressed GetQueueUrl message is a separate constant and not in scope).
Traces: cloudwatch__events__sqs.xsvc-cloudwatch-1-alarm-state-change-chain-delivered-to-sqs.

## Evidence
Representative expected-vs-actual from the replay:

1) cloudwatch__events__sqs.xsvc-cloudwatch-2-alarm-configuration-change-events-route-create-update-delete-only-to-the-matching-rule — AssertionError: the event configuration carries 0 metric entries, expected exactly 1 for a single-metric alarm: {'alarmName': 'sim2real-xsvc-cw2-al-<suffix>', 'alarmArn': 'arn:aws:cloudwatch:<region>:<account>:alarm:sim2real-xsvc-cw2-al-<suffix>', 'actionsEnabled': True, 'okActions': [], 'alarmActions': ['arn:aws:sns:<region>:<account>:sim2real-xsvc-cw2-noop-<suffix>'], 'insufficientDataActions': [], 'namespace': 'Sim2Real/XsvcCw2', 'metricName': 'ConfigChangeProbe', 'statistic': 'Average', 'dimensions': [], 'period': 60, 'evaluationPeriods': 1, 'threshold': 5.0, 'comparisonOperator': 'GreaterThanThreshold', 'treatMissingData': 'missing'}. The check reads configuration['metrics'][0]['metricStat']['metric']['namespace'|'name'] — present in the recording, absent here.

2) cloudwatch__events__sqs.xsvc-cloudwatch-2-state-changes-tagging-and-noop-puts-emit-no-configuration-change-event — 'Check timed out after 60.0s'. No assertion was reached; the check's warm-up requires five CONSECUTIVE PutMetricAlarm writes each delivering its own Configuration Change event (matched by detail.configuration.threshold), any miss resetting the streak.

3) cloudwatch__events__sqs.xsvc-cloudwatch-1-alarm-state-change-chain-delivered-to-sqs — AssertionError: SendMessage sim2real-xsvc-cw1-chain-q-<suffix>: AWS.SimpleQueueService.NonExistentQueue message is 'The specified queue does not exist for this wsdl version.', expected 'The specified queue does not exist.' Source of the constant: services/service-com-amazonaws-sqs-amazonsqs/src/state.rs:3597-3598 (QUEUE_DOES_NOT_EXIST_MESSAGE), doc comment at 3583-3596.

## Steps to Reproduce
Boot the emulator and replay the behavioral suite restricted to the trace_ids on this issue.

Cause 1: sqs:CreateQueue, read its QueueArn, set a queue policy allowing events.amazonaws.com sqs:SendMessage from the rule ARN; events:PutRule with pattern {"source":["aws.cloudwatch"],"detail-type":["CloudWatch Alarm Configuration Change","CloudWatch Alarm State Change"],"detail":{"alarmName":[<alarm>]}} and events:PutTargets to the queue. Then cloudwatch:PutMetricAlarm(AlarmName=<alarm>, Namespace='Sim2Real/XsvcCw2', MetricName='ConfigChangeProbe', Statistic='Average', Period=60, EvaluationPeriods=1, Threshold=5, ComparisonOperator='GreaterThanThreshold', TreatMissingData='missing'). Receive from the queue and inspect detail.configuration: it must contain a one-entry `metrics` array whose metricStat.metric.namespace/name are the alarm's namespace and metric name.

Cause 2: with the same wiring, issue repeated PutMetricAlarm writes that change only Threshold and confirm that EACH write delivers exactly one Configuration Change event carrying that threshold; then issue SetAlarmState, TagResource and a no-op PutMetricAlarm and confirm the detail-types emitted for each.

Cause 3: sqs:CreateQueue, sqs:DeleteQueue, then sqs:SendMessage(QueueUrl=<deleted url>, MessageBody='gone') and compare Error.Code (AWS.SimpleQueueService.NonExistentQueue), status (400) and Error.Message (must be exactly 'The specified queue does not exist.').
