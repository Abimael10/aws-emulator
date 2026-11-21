Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: arc-zonal-shift.practice-run-blocked-by-alarm-in-alarm-state-rejected
Issue-Shape-Id: com.amazonaws.arczonalshift#PercDataPlane
Issue-Key: arc-zonal-shift.practice-run-blocked-by-alarm-in-alarm-state-rejected|run#22|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Commit: c1e629eb7cbb6abd12d76d276265809665f4d5fd

# arc-zonal-shift StartPracticeRun ConflictException message for a red blocking alarm does not match AWS's wording

> _Note: this issue was identified against the code base at commit `c1e629eb7cbb6abd12d76d276265809665f4d5fd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartPracticeRun

## Description
One root cause.

Cause 1 of 1: StartPracticeRun correctly refuses with ConflictException when a configured blocking alarm is in the ALARM state, but the message string it serializes is its own prose rather than the message AWS returns for this conflict class.

AWS: Smithy (services/service-com-amazonaws-arczonalshift-percdataplane) models StartPracticeRun with error ConflictException @httpError 409, whose members are `message` (String, required), `reason` (ConflictExceptionReason, required) and optional `zonalShiftId`. ConflictExceptionReason includes the value `PracticeBlockingAlarmsRed`, which is the reason for this refusal. The model does not document the message text, so for the text this is recorded behavior only: the recording shows AWS answering HTTP 409, code `ConflictException`, with the message `Cannot start practice run when blocking alarms are red`. That message is entirely FIXED text for this error class — it interpolates no value from the request or from state (no resource identifier, no alarm name, no availability zone, no zonal shift id), so there is nothing per-run in it and the whole string is the requirement.

Emulator: for the identical call the emulator answers with code `ConflictException` but the message `Cannot start a practice run because a blocking alarm for the requested resource is in the ALARM state.` The status and the error code are right; only the message string diverges. It is a differently-worded sentence, not a differently-filled template.

Why: a client that branches on, logs, or asserts on the conflict message — the common way to distinguish a blocking-alarm conflict from the other eleven ConflictExceptionReason conflicts when the reason member is not inspected — sees a string it has never seen from AWS. Operator runbooks, alerting rules and test suites that match AWS's wording silently stop matching, and the refusal is misreported as an unknown conflict rather than a red blocking alarm.

Fix: the message constructed by the blocking-alarm precondition in the StartPracticeRun handler of the arc-zonal-shift service crate (the error-construction site that raises ConflictException for the PracticeBlockingAlarmsRed case), not the precondition logic itself — the refusal is correctly detected and correctly classified; only the literal text emitted with it is wrong. While that site is edited, confirm the response also carries `reason = PracticeBlockingAlarmsRed` as the model requires that member.

Traces: arc-zonal-shift.practice-run-blocked-by-alarm-in-alarm-state-rejected. The check first confirms the positive controls pass (StartPracticeRun succeeds and the shift is visible via ListZonalShifts while the alarm is OK, cancel returns CANCELED, no shift is created while blocked, and the identical call succeeds again once the alarm returns to OK) — all of those behaviors are correct in the emulator, which isolates the divergence to the message string alone.

## Evidence
Failing assertion from the replay (check_arc_zonal_shift_practice_run_blocked_by_alarm_in_alarm_state_rejected.py line 69):

  AssertionError: expected message 'Cannot start practice run when blocking alarms are red', got 'Cannot start a practice run because a blocking alarm for the requested resource is in the ALARM state.'

Expected (AWS, from the recording):
  HTTP 409
  Error.Code    = ConflictException
  Error.Message = "Cannot start practice run when blocking alarms are red"   (fixed text in full; no request- or state-derived values)
  reason        = PracticeBlockingAlarmsRed  (Smithy: ConflictException.reason is @required, enum ConflictExceptionReason)

Actual (emulator):
  HTTP 409
  Error.Code    = ConflictException   (correct)
  Error.Message = "Cannot start a practice run because a blocking alarm for the requested resource is in the ALARM state."   (diverges)

Everything else the check asserts passed: baseline StartPracticeRun returned status ACTIVE and was visible in ListZonalShifts, CancelPracticeRun returned CANCELED and removed it, no active shift existed while blocked, and StartPracticeRun succeeded again after the alarm returned to OK.

## Steps to Reproduce
1. Create an ELBv2 NLB managed resource registered with ARC zonal shift and note its ARN and an away-from AZ.
2. Create two CloudWatch alarms in state OK, one to use as the outcome alarm and one as the blocking alarm.
3. arc-zonal-shift CreatePracticeRunConfiguration(resourceIdentifier=<lb arn>, outcomeAlarms=[{type: CLOUDWATCH, alarmIdentifier: <outcome alarm arn>}], blockingAlarms=[{type: CLOUDWATCH, alarmIdentifier: <blocking alarm arn>}]).
4. cloudwatch SetAlarmState(AlarmName=<blocking alarm>, StateValue=ALARM, StateReason=...) and poll DescribeAlarms until StateValue == ALARM.
5. Call arc-zonal-shift StartPracticeRun(resourceIdentifier=<lb arn>, awayFrom=<az>, comment="blocked-attempt").
6. Observe the error: the code is ConflictException with HTTP 409 as expected, but Error.Message is "Cannot start a practice run because a blocking alarm for the requested resource is in the ALARM state." instead of AWS's "Cannot start practice run when blocking alarms are red".
