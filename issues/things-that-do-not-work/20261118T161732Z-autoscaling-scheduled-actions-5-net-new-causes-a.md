Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.asg-suspended-scheduled-actions-skip-an-occurrence-and-resume-without-backfill autoscaling.e2e-scheduled-action-capacity-change-flow autoscaling.scheduled-action-cron-is-interpreted-in-its-declared-time-zone autoscaling.scheduled-action-time-window autoscaling.scheduled-action-time-window-triggers autoscaling.scheduled-action-timestamp-subsecond-preserved autoscaling.put-scheduled-action-malformed-recurrence-rejected autoscaling.op-contract-put-scheduled-update-group-action
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.asg-suspended-scheduled-actions-skip-an-occurrence-and-resume-without-backfill|_poll#19|AssertionError|raise AssertionError(f"{description} not met within {timeout_s:.0f}s; last={last!r}")
Issue-Key: autoscaling.e2e-scheduled-action-capacity-change-flow|_poll_until#21|AssertionError|raise AssertionError(f"condition not met within {timeout_s}s; last={last!r}")
Issue-Key: autoscaling.scheduled-action-cron-is-interpreted-in-its-declared-time-zone|_poll#19|AssertionError|raise AssertionError(f"{description} not met within {timeout_s}s; last={last!r}")
Issue-Key: autoscaling.scheduled-action-time-window|_poll_until#23|AssertionError|raise AssertionError(f"{description} not met within {timeout_s}s; last result: {last!r}")
Issue-Key: autoscaling.scheduled-action-time-window-triggers|run#19|AssertionError|assert expired_gone, (
Issue-Key: autoscaling.scheduled-action-timestamp-subsecond-preserved|run#13|AssertionError|assert got_start.microsecond == 0, (
Issue-Key: autoscaling.put-scheduled-action-malformed-recurrence-rejected|run#10|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: autoscaling.op-contract-put-scheduled-update-group-action|_error#22|AssertionError|raise AssertionError(f"expected the call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Commit: c9344e2779494e078eb181c7552bbea5ec625bd0

# autoscaling scheduled actions: 5 net-new causes — a due action never runs, an expired one never disappears, sub-second times are kept, and two refusals are missing/mis-ordered

> _Note: this issue was identified against the code base at commit `c9344e2779494e078eb181c7552bbea5ec625bd0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutScheduledUpdateGroupAction

## Description
Five net-new root causes in the Auto Scaling scheduled-action plane. Each must be closed separately; they are bundled only because they all belong to PutScheduledUpdateGroupAction / DescribeScheduledActions and the scheduler that executes stored actions in one service.

Cause 1 of 5 — a scheduled action is stored but never executed: when its Time/StartTime/Recurrence occurrence comes due, the group's MinSize/MaxSize/DesiredCapacity are never updated.
AWS: Smithy (com.amazonaws.autoscaling#AutoScaling_2011_01_01): PutScheduledUpdateGroupAction accepts Time/StartTime/EndTime (TimestampType), Recurrence and TimeZone (XmlStringMaxLen255) plus MinSize/MaxSize/DesiredCapacity; the model describes only the stored shape, so the execution semantics are recorded behavior only — AWS applies the action's MinSize/MaxSize/DesiredCapacity to the group at the occurrence instant (per-minute granularity), interpreting a cron Recurrence in the action's declared IANA TimeZone, and the resulting change is observable on DescribeAutoScalingGroups (and drives launches/terminations toward the new desired capacity).
Emulator: the action is accepted and round-trips on DescribeScheduledActions, but nothing ever fires. The groups stay at their pre-action capacity for the whole observation window: DesiredCapacity stays 0 past the occurrence (polls time out after 180-420 s with last=0 / the unchanged group document), and the zone-aware occurrence never moves the group either, so the emulator is not merely mis-interpreting the TimeZone — no occurrence of any kind is executed.
Why: scheduled scaling is unusable. A client that schedules a capacity change and waits for it (or a test/deployment gate that asserts the post-occurrence capacity) waits forever, and a suspension/resumption test of the ScheduledActions process cannot be distinguished from the action simply never running.
Fix: the scheduled-action execution path in the autoscaling service state/engine (the component that must evaluate stored ScheduledUpdateGroupActions against the clock and apply them to the group), plus the TimeZone-aware occurrence computation it needs.
Traces: autoscaling.e2e-scheduled-action-capacity-change-flow, autoscaling.scheduled-action-time-window, autoscaling.scheduled-action-cron-is-interpreted-in-its-declared-time-zone, autoscaling.asg-suspended-scheduled-actions-skip-an-occurrence-and-resume-without-backfill.

Cause 2 of 5 — DescribeScheduledActions keeps returning an action whose window has already elapsed.
AWS: Smithy: DescribeScheduledActions returns ScheduledUpdateGroupActions; recorded behavior only for the retention rule — once a one-time action has run, or its EndTime has passed, AWS stops returning it (the action is no longer scheduled), so a client polling DescribeScheduledActions sees it drop out.
Emulator: an action whose StartTime/EndTime window started and ended seconds after creation is still listed by DescribeScheduledActions for the whole poll window, alongside the still-active action (both names present after the deadline).
Why: a client that reconciles its schedule against DescribeScheduledActions (delete-if-stale, or 'is my action still pending?') sees phantom schedules forever and cannot tell a pending action from a spent one; the per-group action count also never drops, so limit arithmetic drifts.
Fix: the DescribeScheduledActions reader / the scheduled-action store — expire an action once it has run or its EndTime has passed, instead of listing every stored action unconditionally.
Traces: autoscaling.scheduled-action-time-window-triggers.

Cause 3 of 5 — StartTime/EndTime are stored with their sub-second component instead of being truncated to whole seconds.
AWS: Smithy: ScheduledUpdateGroupAction.StartTime/EndTime are TimestampType, documented as 'date and time in UTC ... for example "2019-06-01T00:00:00Z"'. Recorded behavior: a StartTime supplied with a sub-second fraction comes back from DescribeScheduledActions truncated to whole-second granularity (microsecond == 0), not rounded up and not preserved.
Emulator: DescribeScheduledActions echoes the sub-second fraction exactly as sent (the microsecond component of the request value is returned unchanged).
Why: a client that compares the schedule it sent with the schedule AWS reports (idempotent re-put, drift detection, golden comparison) sees a value AWS never serves, and any ordering/equality logic keyed on whole-second times mis-fires.
Fix: PutScheduledUpdateGroupAction's handler (where StartTime/EndTime/Time are normalized before being stored) — truncate the timestamp to whole seconds on write.
Traces: autoscaling.scheduled-action-timestamp-subsecond-preserved.

Cause 4 of 5 — a malformed Recurrence is never validated, and the capacity-bound check pre-empts it.
AWS: Smithy: Recurrence targets XmlStringMaxLen255 with no cron pattern, so the 5-field cron check is a server-side validation whose refusal is recorded behavior only: HTTP 400, awsQueryError code ValidationError, message 'Given recurrence string: <recurrence> is invalid' — fixed text 'Given recurrence string: ' + the request's Recurrence value + ' is invalid'. That check runs before the Min/Max/DesiredCapacity consistency check, and nothing is stored.
Emulator: the request is refused, but for the wrong reason and with a different message: HTTP 400 ValidationError 'Desired capacity:<n> must be between the specified min size:<min> and max size:<max>' — i.e. the capacity-bound validation runs first and the unparseable Recurrence is never examined at all (so with in-range capacities it would be stored as a schedule that can never fire).
Why: the caller is pointed at its capacity numbers while the actual fault is the cron string, so remediation goes down the wrong path; and a client keying on AWS's recurrence sentence never recognizes the refusal.
Fix: PutScheduledUpdateGroupAction's input validation — add the cron-syntax check for Recurrence and order it ahead of the capacity-bound check, emitting AWS's sentence with the request's recurrence interpolated.
Traces: autoscaling.put-scheduled-action-malformed-recurrence-rejected.

Cause 5 of 5 — a StartTime in the past is accepted instead of refused.
AWS: recorded behavior only (the model places no range on StartTime): PutScheduledUpdateGroupAction with a StartTime that is already in the past is refused with HTTP 400, code ValidationError and the wholly fixed sentence 'Given start time is in the past' (no interpolated values), and no scheduled action is created.
Emulator: the call returns HTTP 200 with an empty PutScheduledUpdateGroupActionResponse and the action is created.
Why: a client that relies on the refusal to detect a stale/clock-skewed schedule silently installs an action AWS would have rejected; combined with cause 1 the action then sits in the group's schedule list forever.
Fix: PutScheduledUpdateGroupAction's input validation — compare StartTime/Time against the current time and refuse with AWS's fixed sentence before anything is stored.
Traces: autoscaling.op-contract-put-scheduled-update-group-action.

Related but non-covering open issues: 20261117T053015Z-autoscaling-refusal-messages-and-validation-precedence-diverge-on.md (PutScheduledUpdateGroupAction TimeZone text and DescribeScheduledActions InvalidNextToken tense) and 20261117T053015Z-autoscaling-traffic-source-and-scheduled-action-state-diverges.md — neither carries these checks, and neither concerns the scheduler never executing an occurrence. All group names, action names, capacities and instants quoted below are one run's values filling the templates.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout (service heading '### autoscaling'):
Cause 1 — autoscaling.e2e-scheduled-action-capacity-change-flow: AssertionError 'condition not met within 420.0s' with the last DescribeAutoScalingGroups document showing MinSize 0 / MaxSize 1 / DesiredCapacity 0 and Instances [] long after the action's StartTime. autoscaling.scheduled-action-time-window: 'scheduled action <name> applied once its StartTime window opened not met within 180s', group still at DesiredCapacity 1. autoscaling.scheduled-action-cron-is-interpreted-in-its-declared-time-zone: 'the scheduled action interpreted in Asia/Kolkata moved the subject group's DesiredCapacity to 1 at or after <instant> not met within 249s; last=0'. autoscaling.asg-suspended-scheduled-actions-skip-an-occurrence-and-resume-without-backfill: 'the control's scheduled action fired at <instant> not met within 355s; last=0' — the CONTROL group (ScheduledActions not suspended) never fired either.
Cause 2 — autoscaling.scheduled-action-time-window-triggers: AssertionError 'an action whose EndTime has already passed should no longer be returned by DescribeScheduledActions, but it was still present after the poll deadline: {<expired-name>, <active-name>}'.
Cause 3 — autoscaling.scheduled-action-timestamp-subsecond-preserved: AssertionError 'StartTime sub-second component was not truncated to zero: got datetime(..., 123456)'; the request sent microsecond=123456 and DescribeScheduledActions returned it verbatim.
Cause 4 — autoscaling.put-scheduled-action-malformed-recurrence-rejected: Recurrence='not a cron expression'; expected ValidationError message 'Given recurrence string: not a cron expression is invalid', got 'Desired capacity:1 must be between the specified min size:0 and max size:0'.
Cause 5 — autoscaling.op-contract-put-scheduled-update-group-action: PutScheduledUpdateGroupAction with a past StartTime expected ValidationError 'Given start time is in the past', got HTTP 200 (ResponseMetadata HTTPStatusCode 200, empty body).

## Steps to Reproduce
1. Replay the behavioral suite (simparity-behavioral-aws-v1) against the emulator built from this checkout.
2. Cause 1: create a zero-capacity group with a launch template, PutScheduledUpdateGroupAction with a StartTime ~90 s out (and separately with Recurrence + TimeZone='Asia/Kolkata'), then poll DescribeAutoScalingGroups past the occurrence and read DesiredCapacity — it never changes.
3. Cause 2: PutScheduledUpdateGroupAction with StartTime=now+15 s and EndTime=now+25 s, then poll DescribeScheduledActions past the EndTime — the action is still listed.
4. Cause 3: PutScheduledUpdateGroupAction with StartTime/EndTime carrying microseconds, then read them back from DescribeScheduledActions.
5. Cause 4: on a group with MinSize=MaxSize=0, PutScheduledUpdateGroupAction with Recurrence='not a cron expression' and DesiredCapacity=1; read Error.Code and Error.Message.
6. Cause 5: PutScheduledUpdateGroupAction with a StartTime in the past; read the HTTP status and then DescribeScheduledActions for the action name.
