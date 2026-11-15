Issue-Source: trace-sweep-divergence
Issue-Trace: cwa-alarm-actions-limit
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cwa-alarm-actions-limit
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: monitoring|PutMetricAlarm|4xx->4xx|$.__type
Issue-Related: 20261029T203422Z-monitoring-cloudwatch-alarm-writes-and-pagination-skip-validation-or.md
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# CloudWatch PutMetricAlarm list-length violation raises InvalidParameterValue instead of ValidationException

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutMetricAlarm

## Description
This issue has ONE cause. There is 1 cause to close.

Cause 1 of 1: PutMetricAlarm refuses an over-long AlarmActions list with the wrong exception shape, the wrong query-error header and a non-AWS message.

  AWS: Smithy (com.amazonaws.cloudwatch#GraniteServiceVersion20100801): PutMetricAlarm.AlarmActions targets ResourceList with @length min 0, max 5 (OKActions and InsufficientDataActions carry the same constraint). AWS reports a violation of a modelled list @length as a constraint-validation failure, not a parameter-value failure: the recorded response is HTTP 400 with __type 'com.amazon.coral.validate#ValidationException', response header x-amzn-query-error 'ValidationError;Sender', and message "1 validation error detected: Value '[<the list the client sent, members joined with ', '>]' at 'alarmActions' failed to satisfy constraint: Member must have length less than or equal to <the modelled max, 5>". The FIXED text of that template is "1 validation error detected: Value '[", "]' at '", "' failed to satisfy constraint: Member must have length less than or equal to "; the VARIABLE parts are (a) the list members, taken verbatim from the request in request order, (b) the member path in lowerCamelCase as AWS names it ('alarmActions'), and (c) the modelled maximum from the Smithy @length trait. The particular topic ARNs in the recording are request values that fill the template, not part of it.

  Emulator: HTTP 400 (status matches) but __type 'com.amazon.coral.service#InvalidParameterValueException', x-amzn-query-error 'InvalidParameterValue;Sender', and message 'The collection AlarmActions must not have a size greater than 5.' - a template of the emulator's own invention that drops the rejected value entirely, uses the PascalCase wire member name instead of AWS's lowerCamelCase member path, and classifies a constraint violation as an invalid-parameter-value error.

  Why: the exception shape is part of the contract. An SDK maps com.amazon.coral.validate#ValidationException and com.amazon.coral.service#InvalidParameterValueException to different modelled error types, and the x-amzn-query-error header is what the awsQuery error mapper keys on, so a client that branches on the error class (or on x-amzn-query-error) takes a different path against the emulator than against AWS, and retry/validation classification diverges. The message also no longer names which actions were rejected, so an operator cannot see what was sent.

  Fix: PutMetricAlarm's input validation (the list-@length check and the shared constraint-violation error mapper feeding the awsQuery error serializer) must raise the ValidationException class with AWS's constraint-violation message template for ResourceList @length, the same way it does for scalar @length members.

  Note: the open issue 20261102T112036Z-monitoring-cloudwatch-validationexception-messages-drop-the-offending-value-and.md covers the same FAMILY of formatter faults (enum sets, not-null, empty-string length) for other operations, but the replay does not report it as owning this cluster and it never mentions list-@length on PutMetricAlarm, so this cause is filed separately; a fixer may well close both in one edit to the shared formatter.

  Traces: cwa-alarm-actions-limit#task#1.

## Evidence
Replay cluster service=monitoring|PutMetricAlarm|status=400->400|keys=$.__type,$.message,$headers.x-amzn-query-error (fixture cwa-alarm-actions-limit, step task#1, affected_count 1, owned_by empty).

Expected (recorded AWS): HTTP 400; $.__type = 'com.amazon.coral.validate#ValidationException'; header x-amzn-query-error = 'ValidationError;Sender'; $.message = "1 validation error detected: Value '[arn:aws:sns:us-west-2:000000000000:sim2real-probe-cwa-topic-0, arn:aws:sns:us-west-2:000000000000:sim2real-probe-cwa-topic-1, arn:aws:sns:us-west-2:000000000000:sim2real-probe-cwa-topic-2, arn:aws:sns:us-west-2:000000000000:sim2real-probe-cwa-topic-3, arn:aws:sns:us-west-2:000000000000:sim2real-probe-cwa-topic-4, arn:aws:sns:us-west-2:000000000000:sim2real-probe-cwa-topic-5]' at 'alarmActions' failed to satisfy constraint: Member must have length less than or equal to 5" (the six ARNs are the request's own values filling the template's value slot; the fixed text and the member path 'alarmActions' and the limit 5 are the requirement).

Actual (emulator): HTTP 400; $.__type = 'com.amazon.coral.service#InvalidParameterValueException'; header x-amzn-query-error = 'InvalidParameterValue;Sender'; $.message = 'The collection AlarmActions must not have a size greater than 5.'

Smithy evidence: PutMetricAlarm.AlarmActions -> ResourceList, @length {min: 0, max: 5}; same for OKActions and InsufficientDataActions.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Fixture cwa-alarm-actions-limit, step task#1: issue an awsQuery PutMetricAlarm whose AlarmActions list carries 6 SNS topic ARNs (one more than the modelled maximum of 5), with the rest of the alarm definition valid.
3. Inspect the 400 response: compare $.__type, the x-amzn-query-error response header and $.message against the recorded expectation above.
4. Observed: __type com.amazon.coral.service#InvalidParameterValueException, header InvalidParameterValue;Sender, message 'The collection AlarmActions must not have a size greater than 5.' instead of the ValidationException class and AWS's "<N> validation error detected: Value '[...]' at 'alarmActions' failed to satisfy constraint: Member must have length less than or equal to 5" template.
