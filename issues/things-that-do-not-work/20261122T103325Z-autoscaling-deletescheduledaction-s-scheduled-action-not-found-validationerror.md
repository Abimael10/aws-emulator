Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.delete-scheduled-action-nonexistent-noop autoscaling.scheduled-action-batch-create-delete autoscaling.scheduled-action-delete-single autoscaling.scheduled-action-upsert-lifecycle
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: auto-scaling|DeleteScheduledAction|ValidationError
Issue-Commit: 68c71f6585a425304635c9d8c1cc1d9408beb2ea

# autoscaling DeleteScheduledAction's scheduled-action-not-found ValidationError appends an emulator-authored detail clause to AWS's fixed message

> _Note: this issue was identified against the code base at commit `68c71f6585a425304635c9d8c1cc1d9408beb2ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteScheduledAction

## Description
ONE net-new root cause (Cause 1 of 1), reached by four behavioral checks. It must be closed before this issue is submitted.

=== Cause 1 of 1: DeleteScheduledAction, called on an EXISTING Auto Scaling group with a ScheduledActionName that does not exist on it, is refused with AWS's status and error code but with an extra ' - Scheduled action <name> not found' clause welded onto AWS's fixed sentence. ===
Failing checks: autoscaling.delete-scheduled-action-nonexistent-noop, autoscaling.scheduled-action-batch-create-delete, autoscaling.scheduled-action-delete-single, autoscaling.scheduled-action-upsert-lifecycle

AWS: HTTP 400, Error.Code 'ValidationError'. Smithy (services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json, awsQuery, 2011-01-01): DeleteScheduledAction input DeleteScheduledActionType {AutoScalingGroupName required XmlStringMaxLen255 length 1-255, ScheduledActionName required XmlStringMaxLen255 length 1-255}, empty output, and the ONLY modeled error is ResourceContentionFault (errorKind server, httpError 500, awsQueryError ResourceContention/500). No not-found error is modeled, so this refusal rides the awsQuery service-common ValidationError and its sentence is recorded behavior only. Recorded behavior: when the group exists but the named scheduled action does not (never created, or already deleted), the Error.Message is exactly 'Scheduled action name not found'. Message anatomy: the sentence is entirely FIXED text with NO variable part — the requested ScheduledActionName is NOT echoed anywhere in the message. The same fixed string is what AWS uses for the per-item BatchDeleteScheduledAction FailedScheduledActions.ErrorMessage, where the name is carried by the sibling ScheduledActionName member instead.
Emulator: HTTP 400 and Error.Code 'ValidationError' are correct, but Error.Message is 'Scheduled action name not found - Scheduled action <scheduled-action-name> not found' — AWS's whole fixed sentence followed by a ' - '-separated clause of the emulator's own ('Scheduled action ' / ' not found') that interpolates the requested action name. Replayed instances (the name in each is that run's generated action name, NOT part of the requirement): 'Scheduled action name not found - Scheduled action sim2real-sched-ghost-4be360b36a6b not found', '... sim2real-sched-b-ae5734bcafd3 not found', '... sim2real-behav-sched-57cb92791f92 not found', '... sim2real-behav-asg-sched-a06bcd0b2385 not found'.
Why: ValidationError is Auto Scaling's single generic client-error code, so the message is the only machine-visible discriminator between this refusal and every other client-side refusal of the operation. A caller that matches AWS's exact sentence — including the very common idempotent-teardown pattern that swallows 'Scheduled action name not found' so a repeated DeleteScheduledAction is a no-op — does not recognise its own not-found refusal and aborts the teardown with an unclassified error. Log/alarm pipelines keyed to the AWS string also miss it.
Fix: the DeleteScheduledAction handler's scheduled-action lookup refusal, in the ValidationError message it builds (services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/ops/delete_scheduled_action.rs and any shared not-found refusal helper it calls) — emit AWS's fixed string and stop appending a detail clause / interpolating the action name.
Related open issues that do NOT cover this path (the replay marked none of these four checks OWNED or ALREADY FILED): 20261122T074644Z-autoscaling-not-found-refusal-detail-text-is-emulator.md covers the DeleteScheduledAction / BatchDeleteScheduledAction GROUP-not-found sentence ('AutoScalingGroup name not found - null') and the BatchDeleteScheduledAction per-item ErrorMessage, but NOT the DeleteScheduledAction action-not-found refusal on an existing group, and it carries wire replay keys only; 20261118T161732Z-autoscaling-scheduled-actions-5-net-new-causes-a.md concerns scheduled-action execution, expiry, sub-second times and two other refusal messages; the stub issue 20261118T161732Z-autoscaling-deletescheduledaction-stub.md does not cover it either — the operation now answers 400 with a ValidationError, so the defect is the message it writes, not a missing handler.
Traces: autoscaling.delete-scheduled-action-nonexistent-noop (deletes a never-created name on an existing group; also has a positive control that an existing action deletes cleanly, so a blanket rejection cannot pass), autoscaling.scheduled-action-delete-single (re-deletes an action it just deleted), autoscaling.scheduled-action-batch-create-delete (deletes one of two BatchPutScheduledUpdateGroupAction-created actions twice), autoscaling.scheduled-action-upsert-lifecycle (re-deletes a gone action at the end of the upsert lifecycle).

## Evidence
Replay of behavioral suite simparity-behavioral-aws-v1 against the booted emulator: 363/465 checks passed (78.1%), 89 mismatched, 13 errored, bootstrap_signal false. Service heading '### autoscaling': 218/277 (79%), 59 failed; 55 of those are marked OWNED by an open issue or ALREADY FILED against an unimplemented stub and are skipped. The four below carry no owner and no stub, and all four fail on the SAME string.

[mismatch] autoscaling.delete-scheduled-action-nonexistent-noop
  check_autoscaling_delete_scheduled_action_nonexistent_noop.py line 61, in run
    assert message == "Scheduled action name not found"
  AssertionError: expected message 'Scheduled action name not found', got 'Scheduled action name not found - Scheduled action sim2real-sched-ghost-4be360b36a6b not found'

[mismatch] autoscaling.scheduled-action-delete-single
  check_autoscaling_scheduled_action_delete_single.py line 66, in run
    assert message == "Scheduled action name not found"
  AssertionError: unexpected message re-deleting a gone scheduled action: 'Scheduled action name not found - Scheduled action sim2real-behav-sched-57cb92791f92 not found'

[mismatch] autoscaling.scheduled-action-batch-create-delete
  check_autoscaling_scheduled_action_batch_create_delete.py line 118, in run
    assert message == "Scheduled action name not found"
  AssertionError: unexpected ValidationError message: 'Scheduled action name not found - Scheduled action sim2real-sched-b-ae5734bcafd3 not found'

[mismatch] autoscaling.scheduled-action-upsert-lifecycle
  check_autoscaling_scheduled_action_upsert_lifecycle.py line 258, in run
    assert message == "Scheduled action name not found"
  AssertionError: unexpected ValidationError message: 'Scheduled action name not found - Scheduled action sim2real-behav-asg-sched-a06bcd0b2385 not found'

In all four, HTTP 400 and Error.Code 'ValidationError' matched — only the message diverged. Required text: the fixed string 'Scheduled action name not found' in full, with no ' - ' clause and no occurrence of the request's ScheduledActionName. The four sim2real-* names above are per-run generated names and are not part of the requirement.

Smithy consulted: DeleteScheduledAction (input {AutoScalingGroupName required length 1-255, ScheduledActionName required length 1-255}, empty output, errors: ResourceContentionFault only -> httpError 500, awsQueryError ResourceContention). No not-found error modeled, so the refusal is the service-common ValidationError and the sentence is recorded behavior only.

## Steps to Reproduce
1. git_clone the repo and run the behavioral suite (simparity-behavioral-aws-v1); the four checks in trace_ids reproduce under the '### autoscaling' heading.
2. Create a launch template and a zero-capacity Auto Scaling group <asg>.
3. Positive control: PutScheduledUpdateGroupAction(AutoScalingGroupName=<asg>, ScheduledActionName=<good>, StartTime=<far future>, DesiredCapacity=0), then DeleteScheduledAction(AutoScalingGroupName=<asg>, ScheduledActionName=<good>). Expect HTTP 200 and the action gone from DescribeScheduledActions.
4. Call DeleteScheduledAction(AutoScalingGroupName=<asg>, ScheduledActionName=<ghost>) where <ghost> was never created on <asg> (or re-issue the step-3 delete for <good>, now gone).
5. Expect HTTP 400, Error.Code 'ValidationError', Error.Message exactly 'Scheduled action name not found' — no ' - ' separator, no detail clause, and the requested action name must not appear in the message.
   Observed: 'Scheduled action name not found - Scheduled action <ghost> not found'.
6. Confirm the group is otherwise unchanged (DescribeScheduledActions still lists only the actions that exist), then re-replay the four checks in trace_ids.
