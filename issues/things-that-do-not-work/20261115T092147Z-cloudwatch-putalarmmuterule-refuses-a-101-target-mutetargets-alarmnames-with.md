Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.put-alarm-mute-rule-target-limit-exceeded-rejected
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch.put-alarm-mute-rule-target-limit-exceeded-rejected|run#17|AssertionError|assert err["Code"] == "ValidationError", f"expected ValidationError for 101 targets, got {err!r}"
Issue-Commit: c49ab90e9518fca4a0d62f5c8e5e4d50d5a70ed9

# PutAlarmMuteRule refuses a 101-target MuteTargets.AlarmNames with InvalidParameterValue instead of AWS's ValidationError constraint refusal

> _Note: this issue was identified against the code base at commit `c49ab90e9518fca4a0d62f5c8e5e4d50d5a70ed9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutAlarmMuteRule

## Description
One net-new root cause (Cause 1 of 1).

Cause 1 of 1: a modeled @length violation on PutAlarmMuteRule.MuteTargets.AlarmNames is refused with the wrong error code and a non-AWS message.

AWS: Smithy (services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json) models PutAlarmMuteRule input member MuteTargets -> MuteTargets.AlarmNames as required with @length {min: 0, max: 100}; its documentation states "The list of alarm names that this mute rule targets. You can specify up to 100 alarm names." and the MuteTargets shape doc states "You can target up to 100 specific alarms by name." The only modeled error on PutAlarmMuteRule is LimitExceededFault (awsQueryError code LimitExceeded, HTTP 400); a constraint violation is NOT that error. CloudWatch is an awsQuery service, and for an input that violates a modeled member constraint it answers HTTP 400 with an Error envelope carrying Type `Sender`, Code `ValidationError`, and the standard framework message: the fixed text `1 validation error detected: Value '` + `' at '` + `' failed to satisfy constraint: Member must have length less than or equal to ` with three variable parts taken from the request and the model — the submitted value (the list of alarm names as sent, comma+space joined inside square brackets), the member path `muteTargets.alarmNames`, and the modeled maximum `100`. No single run's alarm names, request id or timestamp are part of the requirement; only the fixed text, the member path and the limit are.

Emulator: the same 101-element request is refused with Code `InvalidParameterValue` (QueryErrorCode `InvalidParameterValueException`), Type `Sender`, and Message `The collection MuteTargets.AlarmNames must not have a size greater than 100.` — a generic container-size message that names the Smithy member in PascalCase (`MuteTargets.AlarmNames`) rather than AWS's lowerCamel request path, and that does not echo the rejected value or AWS's "1 validation error detected" preamble. Replay detail: `AssertionError: expected ValidationError for 101 targets, got {'Message': 'The collection MuteTargets.AlarmNames must not have a size greater than 100.', 'Code': 'InvalidParameterValue', 'QueryErrorCode': 'InvalidParameterValueException', 'Type': 'Sender'}`.

Why: a client that branches on the botocore error code sees `InvalidParameterValue` where AWS reports `ValidationError`, so constraint-violation handling (surfacing which member failed, distinguishing a malformed request from a bad parameter value, and the non-retryable classification clients attach to ValidationError) takes the wrong branch. The message also does not identify the offending request member path, so a caller cannot locate the bad field programmatically. Because the refusal diverges at the first assertion, the rest of the check's contract (a rejected create stores no rule, a rejected update leaves the stored targets and Duration unchanged, and an update to a different 100 targets replaces them) was never exercised and may hide further divergence.

Fix: the length/constraint validation and the awsQuery error serializer for PutAlarmMuteRule in the cloudwatch service crate — the handler's input validation must raise the ValidationError class (HTTP 400, Code ValidationError, Type Sender) with AWS's "1 validation error detected: ... failed to satisfy constraint: Member must have length less than or equal to <max>" wording and the lowerCamel member path, rather than the generic collection-size InvalidParameterValue path. The boundary case of exactly 100 targets must continue to be accepted.

Traces: cloudwatch.put-alarm-mute-rule-target-limit-exceeded-rejected.

Note: the open issue 20261029T182452Z-cloudwatch-request-validation-and-error-envelope-divergences-10.md covers other cloudwatch request-validation/envelope divergences and the replay did NOT mark this check as owned by it; this cause is filed separately and that issue is named here for cross-reference. The unimplemented-DeleteAlarmMuteRule cluster (services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/ops/delete_alarm_mute_rule.rs) is already filed and is a different cause: this check reached PutAlarmMuteRule and got a served refusal.

## Evidence
Check: cloudwatch.put-alarm-mute-rule-target-limit-exceeded-rejected (verdict: mismatch).

Request: PutAlarmMuteRule with Name=<new rule>, Rule={Schedule:{Expression:'at(2099-01-01T00:00)', Duration:'PT1H', Timezone:'UTC'}}, MuteTargets={AlarmNames:[<101 existing alarm names>]}.

Expected (AWS): HTTP 400, Error Type Sender, Code `ValidationError`, Message `1 validation error detected: Value '[<the 101 submitted alarm names, comma+space joined>]' at 'muteTargets.alarmNames' failed to satisfy constraint: Member must have length less than or equal to 100`. Fixed text: "1 validation error detected: Value '", "' at '", "' failed to satisfy constraint: Member must have length less than or equal to ". Variable parts: the submitted name list (from the request), the member path `muteTargets.alarmNames` (from the model), the limit `100` (from the model's @length max).

Actual (emulator): Error {'Message': 'The collection MuteTargets.AlarmNames must not have a size greater than 100.', 'Code': 'InvalidParameterValue', 'QueryErrorCode': 'InvalidParameterValueException', 'Type': 'Sender'}.

Model citation: Smithy MuteTargets.AlarmNames -> MuteTargetAlarmNameList, required, @length {min:0, max:100}; PutAlarmMuteRule errors: [LimitExceededFault (awsQueryError LimitExceeded, HTTP 400)] only.

## Steps to Reproduce
1. Create 101 metric alarms in one namespace (PutMetricAlarm with MetricName=MuteTarget, Statistic=Average, Period=60, EvaluationPeriods=1, Threshold=1.0, ComparisonOperator=GreaterThanThreshold).
2. Call PutAlarmMuteRule with a schedule rule and MuteTargets={'AlarmNames': <the first 100 names>} — this must be accepted, and GetAlarmMuteRule must read back exactly those 100 names.
3. Call PutAlarmMuteRule with a new Name and MuteTargets={'AlarmNames': <all 101 names>}.
4. Inspect the error envelope: AWS answers HTTP 400 Code=ValidationError with the "1 validation error detected: ... at 'muteTargets.alarmNames' failed to satisfy constraint: Member must have length less than or equal to 100" message; the emulator answers Code=InvalidParameterValue with "The collection MuteTargets.AlarmNames must not have a size greater than 100."
5. Re-replay check cloudwatch.put-alarm-mute-rule-target-limit-exceeded-rejected to confirm the fix (it also asserts that the rejected create stores no rule and that a rejected update leaves the stored targets and Duration unchanged).
