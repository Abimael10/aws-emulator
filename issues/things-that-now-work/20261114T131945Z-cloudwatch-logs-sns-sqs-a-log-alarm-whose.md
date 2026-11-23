Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__logs__sns__sqs.deep-cloudwatch-2-cwlogalarm-unassumable-query-role-evaluation-error
Issue-Key: cloudwatch__logs__sns__sqs.deep-cloudwatch-2-cwlogalarm-unassumable-query-role-evaluation-error|run#24|AssertionError|assert bad.get("EvaluationState") == "EVALUATION_ERROR", (
Issue-Commit: 2860ab852527d3986692e1f6aab0d3815fee8ec2

# cloudwatch__logs__sns__sqs: a log alarm whose ScheduledQueryRoleARN cannot be assumed still runs its scheduled query, reaching ALARM instead of EvaluationState=EVALUATION_ERROR

> _Note: this issue was identified against the code base at commit `2860ab852527d3986692e1f6aab0d3815fee8ec2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutLogAlarm

## Description
1 net-new cause.

Cause 1 of 1: the scheduled-query evaluation of a LogAlarm never assumes the configured ScheduledQueryRoleARN, so a role that CloudWatch cannot assume is indistinguishable from an assumable one and the alarm evaluates (and notifies) anyway.
Failing check ids: cloudwatch__logs__sns__sqs.deep-cloudwatch-2-cwlogalarm-unassumable-query-role-evaluation-error

AWS:      Smithy (com.amazonaws.cloudwatch): ScheduledQueryConfiguration.ScheduledQueryRoleARN is @required and documented as "The Amazon Resource Name (ARN) of the IAM role that CloudWatch assumes when executing the scheduled query against the configured log groups." The role is NOT validated at call time — PutLogAlarm has only LimitExceededFault (400/LimitExceeded) and ResourceConflict (409) modelled, neither of which covers an unassumable role — so PutLogAlarm returns 200 and the alarm starts in StateValue=INSUFFICIENT_DATA. The failure surfaces asynchronously on the first scheduled evaluation: LogAlarm.EvaluationState is set to EVALUATION_ERROR, which the model documents as "configuration errors in the alarm setup that require review and correction. Refer to the StateReason field of the alarm for more details" (EvaluationState enum values: PARTIAL_DATA | EVALUATION_FAILURE | EVALUATION_ERROR). Because the query never ran, the alarm does NOT transition: StateValue stays INSUFFICIENT_DATA, the per-result entries in StateReasonData carry status "FAILED" and NO "value" member (there is no aggregation to report), and no AlarmActions notification is published. StateUpdatedTimestamp is documented as the time stamp of the last update to StateValue *or EvaluationState*, so an evaluation error is a state update even without a StateValue change. Repairing only the role's trust policy (iam:UpdateAssumeRolePolicy to trust the CloudWatch scheduled-query principal) must let the SAME alarm recover: EvaluationState is cleared (the member is absent), results return with status "COMPLETE" and a computed value, and the alarm transitions normally. The value in StateReasonData and the numbers inside StateReason are computed from the log events in the query window and the alarm's Threshold — they are not fixed text.

Emulator: the replay creates two otherwise identical log alarms over the same log group and schedule — one whose role trusts the CloudWatch scheduled-query principal, one whose role trusts only ec2.amazonaws.com and therefore cannot be assumed. Both are accepted with 200 and both then EVALUATE. DescribeAlarms(AlarmTypes=["LogAlarm"]) for the unassumable-role alarm returns EvaluationState absent (None) and StateValue='ALARM' with StateReason='Threshold Crossed: 1 out of the last 2 query results [7.0] was greater than or equal to the threshold (3.0) (minimum 1 query result for OK -> ALARM transition).' — i.e. the emulator ran the query under a role it could not have assumed, computed the count(*) aggregation, crossed the threshold and fired. The ScheduledQueryRoleARN is stored and echoed back correctly by DescribeAlarms (the check asserts that and it passes), so the ARN is persisted but never used as an authorization input at evaluation time. Consequently EVALUATION_ERROR is unreachable for this cause, StateReasonData contains COMPLETE results with values instead of FAILED results, and the AlarmActions SNS publish happens for an alarm AWS would never have evaluated.

Why:      a client relying on AWS's behaviour cannot detect a misconfigured log alarm. The documented operator workflow is exactly "poll DescribeAlarms, see EvaluationState=EVALUATION_ERROR, read StateReason, fix the trust policy": against the emulator that signal never appears, so a broken alarm looks healthy. Worse, the failure mode inverts: instead of withholding notifications the emulator sends them, so alerting pipelines and runbooks built on log alarms fire spurious ALARM notifications (and their downstream SNS/SQS consumers act on them) from alarms whose queries real CloudWatch could not execute at all. Anyone testing the repair path also gets a false pass, because the alarm was never in the error state to recover from.

Fix:      the LogAlarm scheduled-query evaluator in the CloudWatch service (the component that drives log-alarm evaluation and the state machine behind DescribeAlarms' LogAlarms, alongside the PutLogAlarm handler that stores ScheduledQueryConfiguration). It must attempt to assume ScheduledQueryConfiguration.ScheduledQueryRoleARN against the IAM state's assume-role trust policy before running the query, and on refusal record EvaluationState=EVALUATION_ERROR, leave StateValue at INSUFFICIENT_DATA, emit FAILED result entries without a "value" member in StateReasonData, suppress all alarm actions, and clear EvaluationState on the next evaluation that succeeds after the trust policy is repaired. PutLogAlarm itself must keep returning 200 — the role is not an input-validation concern.

Traces:   cloudwatch__logs__sns__sqs.deep-cloudwatch-2-cwlogalarm-unassumable-query-role-evaluation-error (assertion at check line 104: "an unassumable query role must yield EvaluationState EVALUATION_ERROR, got None state='ALARM'"). No open issue covers this: the existing cloudwatch__logs / cloudwatch__logs__transfer alarm issues concern metric-filter datapoints, PutTransformer parsers and CFN-created metric alarms being invisible to DescribeAlarms, none of which touch log-alarm scheduled-query role assumption or EvaluationState.

## Evidence
Check: cloudwatch__logs__sns__sqs.deep-cloudwatch-2-cwlogalarm-unassumable-query-role-evaluation-error [mismatch]

Setup (from the replayed check): one log group with stream 's' and 7 log events matching the query; two IAM roles, both carrying logs:StartQuery/StopQuery/GetQueryResults/DescribeLogGroups, differing ONLY in their trust policy — 'good' trusts logs.amazonaws.com, 'bad' trusts ec2.amazonaws.com. Two PutLogAlarm calls with identical ScheduledQueryConfiguration (QueryString "fields @timestamp, @message | filter @message like /BREACH/", AggregationExpression "count(*)", rate(1 minute), StartTimeOffset 300), QueryResultsToEvaluate=2, QueryResultsToAlarm=1, Threshold=3.0, ComparisonOperator=GreaterThanOrEqualToThreshold, AlarmActions=[topic].

Expected (AWS):
  PutLogAlarm(bad role)            -> 200 (role not assumed at call time)
  DescribeAlarms(LogAlarm) initial -> StateValue=INSUFFICIENT_DATA
  after first evaluation           -> EvaluationState="EVALUATION_ERROR"
                                      StateValue="INSUFFICIENT_DATA"
                                      StateReasonData results: every entry status="FAILED", no "value" member
                                      no SNS notification for this alarm
  positive control (good role)     -> StateValue="ALARM", EvaluationState absent, a computed value of 7.0, notification delivered
  after repairing ONLY the trust policy -> same alarm reaches ALARM, EvaluationState member absent, results status="COMPLETE" with a value >= Threshold

Actual (emulator):
  PutLogAlarm(bad role)            -> 200 (matches)
  DescribeAlarms(LogAlarm) initial -> StateValue=INSUFFICIENT_DATA, ScheduledQueryRoleARN echoed = the bad role (matches)
  after first evaluation           -> EvaluationState = absent (None)
                                      StateValue = "ALARM"
                                      StateReason = "Threshold Crossed: 1 out of the last 2 query results [7.0] was greater than or equal to the threshold (3.0) (minimum 1 query result for OK -> ALARM transition)."
                                      -> the query RAN and the aggregation (7.0) was computed under a role that cannot be assumed; the alarm transitioned and its AlarmActions path was taken.

Assertion that failed (check line 104):
  AssertionError: an unassumable query role must yield EvaluationState EVALUATION_ERROR, got None state='ALARM' reason='Threshold Crossed: 1 out of the last 2 query results [7.0] was greater than or equal to the threshold (3.0) (minimum 1 query result for OK -> ALARM transition).'

Message-template note: in the expected error surface, the requirement is the enum value EvaluationState="EVALUATION_ERROR" and the result status "FAILED" (fixed text, from the Smithy EvaluationState enum). The counts, the "[7.0]", the "(3.0)" threshold and the alarm names in the StateReason above are values derived from this run's log events, Threshold and generated names — they fill the template and are not part of it.

Model citations: Smithy com.amazonaws.cloudwatch — ScheduledQueryConfiguration.ScheduledQueryRoleARN (@required, "the IAM role that CloudWatch assumes when executing the scheduled query"); LogAlarm.EvaluationState (enum PARTIAL_DATA|EVALUATION_FAILURE|EVALUATION_ERROR, EVALUATION_ERROR = "configuration errors in the alarm setup that require review and correction"); LogAlarm.StateUpdatedTimestamp ("last update to the value of either the StateValue or EvaluationState parameters"); PutLogAlarm errors = LimitExceededFault(400/LimitExceeded), ResourceConflict(409) only.

## Steps to Reproduce
1. Create an SNS topic with an SQS queue subscribed (queue policy allowing sns.amazonaws.com with aws:SourceArn = the topic).
2. Create a CloudWatch Logs log group with one log stream, and two IAM roles that both allow logs:StartQuery, logs:StopQuery, logs:GetQueryResults and logs:DescribeLogGroups on "*", differing only in their AssumeRolePolicyDocument:
     good: Principal {"Service": ["logs.amazonaws.com"]}
     bad:  Principal {"Service": ["ec2.amazonaws.com"]}   <- CloudWatch cannot assume this
3. PutLogEvents 7 events into the stream whose messages match the query filter.
4. Call PutLogAlarm twice (once per role), identical apart from AlarmName and ScheduledQueryConfiguration.ScheduledQueryRoleARN:
     ScheduledQueryConfiguration = { QueryString: "fields @timestamp, @message | filter @message like /BREACH/",
                                     LogGroupIdentifiers: [<group>],
                                     ScheduledQueryRoleARN: <good|bad role arn>,
                                     ScheduleConfiguration: { ScheduleExpression: "rate(1 minute)", StartTimeOffset: 300, EndTimeOffset: 0 },
                                     AggregationExpression: "count(*)" }
     QueryResultsToEvaluate=2, QueryResultsToAlarm=1, Threshold=3.0,
     ComparisonOperator="GreaterThanOrEqualToThreshold", AlarmActions=[<topic arn>]
   Both must return 200.
5. Poll DescribeAlarms(AlarmNames=[<bad alarm>], AlarmTypes=["LogAlarm"]) until it reports an evaluation outcome.
   EXPECT EvaluationState == "EVALUATION_ERROR", StateValue == "INSUFFICIENT_DATA", every entry in json.loads(StateReasonData)["results"] with status "FAILED" and no "value" key, and no alarm notification for that alarm on the queue.
   OBSERVED EvaluationState absent and StateValue == "ALARM" with a computed query value.
6. Confirm the differential: the good-role alarm reaches ALARM with a computed value of 7.0 and delivers a notification — the emulator produces the same outcome for BOTH alarms, which is the divergence.
7. Repair only the bad role's trust policy (UpdateAssumeRolePolicy to trust logs.amazonaws.com), PutLogEvents another 7 matching events, and poll the same alarm: it must reach ALARM with EvaluationState absent and results status "COMPLETE" — i.e. the recovery transition must be observable, which it cannot be while step 5 never enters the error state.
