Issue-Source: trace-sweep-divergence
Issue-Trace: autoscaling-delete-launch-configuration-never-created
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: autoscaling-delete-launch-configuration-never-created autoscaling-launch-config-and-account-level-policy-reads autoscaling-resource-creation-validation-errors autoscaling-policy-schedule-hook-notification-tag-lifecycle autoscaling-scheduled-actions-on-nonexistent-group autoscaling-batch-delete-scheduled-action-partial-failure-envelope
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling|DeleteLaunchConfiguration|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|DeleteScheduledAction|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|BatchDeleteScheduledAction|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|BatchDeleteScheduledAction|2xx->2xx|$.BatchDeleteScheduledActionResponse.BatchDeleteScheduledActionResult.FailedScheduledActions.member.ErrorMessage
Issue-Commit: ea39dba1da951c7945754830175526ec44cfb5ae

# autoscaling not-found refusal detail text is emulator-authored on launch-configuration and scheduled-action paths

> _Note: this issue was identified against the code base at commit `ea39dba1da951c7945754830175526ec44cfb5ae`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteLaunchConfiguration

## Description
FOUR net-new root causes, all the same shape: the emulator gets the HTTP status and the error code right and then writes its own sentence after AWS's fixed '<resource> name not found' prefix, inventing a detail clause AWS does not use and interpolating a resource name into positions AWS leaves fixed. This bundle carries 4 causes and ALL FOUR must be closed before it is submitted.

Cause 1 of 4 — DeleteLaunchConfiguration's not-found detail clause is re-worded.
AWS: HTTP 400, Error.Code 'ValidationError'. Smithy (services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json, awsQuery, 2011-01-01) models DeleteLaunchConfiguration with LaunchConfigurationName required and only ResourceInUseFault (awsQueryError ResourceInUse/400) and ResourceContentionFault (500) as errors; no not-found error is modeled, so the refusal rides the awsQuery service-common ValidationError and its sentence is recorded behavior only. Recorded behavior: deleting a launch configuration that was never created is refused with Error.Message 'Launch configuration name not found - Launch configuration <launch-configuration-name> not found'. Message anatomy: fixed text 'Launch configuration name not found - Launch configuration ', then ONE variable part — the LaunchConfigurationName copied verbatim from the request — then the fixed text ' not found'.
Emulator: HTTP 400 and Error.Code 'ValidationError' are correct, but it answers 'Launch configuration name not found - A launch configuration with the name: <name> does not exist' — it keeps AWS's prefix and replaces the whole detail clause with prose of its own ('A launch configuration with the name: ' / ' does not exist').
Why: ValidationError is Auto Scaling's single generic client-error code, so the message is the only machine-visible discriminator between the not-found and in-use refusals on this operation. Any client, test or log pipeline matching AWS's sentence sees an unrecognised failure, and an idempotent delete that treats the exact not-found text as success instead reports a hard error.
Fix: the DeleteLaunchConfiguration handler's not-found refusal, in the ValidationError message it builds — emit AWS's detail clause verbatim around the request's name.
Traces: autoscaling-delete-launch-configuration-never-created#task#2 (representative), autoscaling-launch-config-and-account-level-policy-reads#task#3, autoscaling-resource-creation-validation-errors#task#4.

Cause 2 of 4 — DeleteScheduledAction's group-not-found refusal interpolates the group name where AWS emits a fixed 'null'.
AWS: HTTP 400, Error.Code 'ValidationError'. Smithy models DeleteScheduledAction with AutoScalingGroupName and ScheduledActionName both required and only ResourceContentionFault (500) as a modeled error, so this refusal rides the service-common ValidationError and its sentence is recorded behavior only. Recorded behavior: when AutoScalingGroupName names no existing group, AWS refuses with Error.Message 'AutoScalingGroup name not found - null'. Message anatomy: the sentence is entirely FIXED text — the detail clause after the ' - ' separator is the literal 'null', with NO variable part; in particular the group name is NOT echoed on this path.
Emulator: HTTP 400 and Error.Code 'ValidationError' are correct, but it answers 'AutoScalingGroup name not found - AutoScalingGroup <auto-scaling-group-name> not found' — it substitutes a sentence of its own for the fixed detail and leaks the requested group name into it.
Why: the message is the only discriminator under the generic ValidationError code, so a caller keyed to AWS's text does not recognise its own group-not-found refusal. A scheduled-action teardown that swallows the exact AWS not-found sentence to stay idempotent instead aborts.
Fix: the DeleteScheduledAction handler's group-lookup refusal — emit AWS's fixed sentence and stop interpolating the group name into the detail clause.
Traces: autoscaling-policy-schedule-hook-notification-tag-lifecycle#task#8 (representative) and autoscaling-scheduled-actions-on-nonexistent-group#task#3.

Cause 3 of 4 — BatchDeleteScheduledAction's group-not-found refusal carries the same invented detail clause.
AWS: HTTP 400, Error.Code 'ValidationError'. Smithy models BatchDeleteScheduledAction with AutoScalingGroupName and ScheduledActionNames both required and only ResourceContentionFault (500) as a modeled error; the client refusal is the service-common ValidationError and its sentence is recorded behavior only. Recorded behavior: a group that does not exist makes the WHOLE batch fail — not a per-item failure — with the identical fixed sentence 'AutoScalingGroup name not found - null' (no variable part).
Emulator: HTTP 400 and Error.Code 'ValidationError' are correct, but it answers 'AutoScalingGroup name not found - AutoScalingGroup <auto-scaling-group-name> not found'. This is the same defect as cause 2 reached through a second handler, so fixing one handler does not fix the other: both must be closed.
Why: as cause 2 — the message is the only discriminator, and here it is also what distinguishes a whole-batch group refusal from the per-item FailedScheduledActions envelope of cause 4.
Fix: the BatchDeleteScheduledAction handler's group-lookup refusal (ideally the shared group-lookup refusal helper these two operations use), emitting AWS's fixed sentence.
Traces: autoscaling-policy-schedule-hook-notification-tag-lifecycle#task#10 (representative) and autoscaling-scheduled-actions-on-nonexistent-group#task#4.

Cause 4 of 4 — BatchDeleteScheduledAction's per-item FailedScheduledActions.ErrorMessage appends an invented detail clause.
AWS: HTTP 200 — this is a partial-failure envelope, not a refusal. Smithy models the output BatchDeleteScheduledActionAnswer with FailedScheduledActions: list of FailedScheduledUpdateGroupActionRequest{ScheduledActionName (required), ErrorCode, ErrorMessage}, documented as 'the error message accompanying the error code'; the value is recorded behavior only. Recorded behavior: for an existing group, a ScheduledActionName in the request that does not exist is reported as one FailedScheduledActions entry whose ErrorMessage is exactly 'Scheduled action name not found'. Message anatomy: entirely FIXED text with NO variable part — the failing action's name is carried by the sibling ScheduledActionName member, not repeated in the message.
Emulator: HTTP 200 and the envelope shape are correct, but the entry's ErrorMessage is 'Scheduled action name not found - Scheduled action <scheduled-action-name> not found' — AWS's whole message with a ' - '-separated clause of the emulator's own appended, duplicating the name already present in ScheduledActionName.
Why: a batch caller reads per-item ErrorMessage to classify which names failed and why; an exact-match classification against AWS's string falls through to 'unknown error' for every missing action, so a partial success is reported as an unclassified failure.
Fix: the BatchDeleteScheduledAction handler's per-item failure construction (the FailedScheduledUpdateGroupActionRequest it builds) — set ErrorMessage to AWS's fixed string and leave the name to ScheduledActionName.
Traces: autoscaling-batch-delete-scheduled-action-partial-failure-envelope#task#6.

Related but NOT covering any of the four: open issue 20261118T161732Z-autoscaling-scheduled-actions-5-net-new-causes-a.md concerns scheduled-action execution, expiry and sub-second times plus two other refusal messages; open issue 20261114T180324Z-autoscaling-group-scoped-not-found-refusals-re-word.md and 20261119T061622Z-autoscaling-validationerror-refusal-detail-text-is-emulator-authored.md cover the same class of re-worded detail text on OTHER operations. None of them names these four operations' sentences and none carries these fixture steps, so a fixer replaying them never exercises these paths.

## Evidence
Wire replay of trace set simparity-wire-aws-v1 against this checkout (emulator_up=true, bootstrap_signal=false; 976/1066 steps passed, 39 divergence clusters). All four clusters below report stub_path='' and owned_by=[] — no unimplemented operation blocks them and no open issue carries their replay key.

Cause 1 — cluster service=autoscaling|DeleteLaunchConfiguration|status=400->400, representative autoscaling-delete-launch-configuration-never-created#task#2, 3 affected steps:
  expected_status 400, actual_status 400; the error code matched, only the message key diverged
  $.ErrorResponse.Error.Message: value_mismatch (expected 'Launch configuration name not found - Launch configuration sim2real-probe-autoscaling-lc-ghost-58028d64 not found', got 'Launch configuration name not found - A launch configuration with the name: sim2real-probe-autoscaling-lc-ghost-58028d64 does not exist')
  Anatomy: required fixed text 'Launch configuration name not found - Launch configuration ' ... ' not found' around the request's LaunchConfigurationName. 'sim2real-probe-autoscaling-lc-ghost-58028d64' is that run's name, not part of the requirement. The emulator's 'A launch configuration with the name: ' / ' does not exist' is its own prose.

Cause 2 — cluster service=autoscaling|DeleteScheduledAction|status=400->400, representative autoscaling-policy-schedule-hook-notification-tag-lifecycle#task#8, 2 affected steps:
  expected_status 400, actual_status 400
  $.ErrorResponse.Error.Message: value_mismatch (expected 'AutoScalingGroup name not found - null', got 'AutoScalingGroup name not found - AutoScalingGroup sim2real-probe-autoscaling-missing-asg not found')
  Anatomy: the expected message is fixed in full, detail clause literally 'null', NO variable part. 'sim2real-probe-autoscaling-missing-asg' is the fixture's missing-group name and must not appear in the message at all.

Cause 3 — cluster service=autoscaling|BatchDeleteScheduledAction|status=400->400, representative autoscaling-policy-schedule-hook-notification-tag-lifecycle#task#10, 2 affected steps:
  expected_status 400, actual_status 400
  $.ErrorResponse.Error.Message: value_mismatch (expected 'AutoScalingGroup name not found - null', got 'AutoScalingGroup name not found - AutoScalingGroup sim2real-probe-autoscaling-missing-asg not found')
  Same anatomy as cause 2, reached through a second handler (distinct divergence cluster and distinct fixture step).

Cause 4 — cluster service=autoscaling|BatchDeleteScheduledAction|status=200->200, representative autoscaling-batch-delete-scheduled-action-partial-failure-envelope#task#6, 1 affected step:
  expected_status 200, actual_status 200 — the partial-failure envelope itself is correct
  $.BatchDeleteScheduledActionResponse.BatchDeleteScheduledActionResult.FailedScheduledActions.member.ErrorMessage: value_mismatch (expected 'Scheduled action name not found', got 'Scheduled action name not found - Scheduled action sim2real-wire-asg-rep-sched-bogus-d12858ad not found')
  Anatomy: the expected ErrorMessage is fixed in full with NO variable part. 'sim2real-wire-asg-rep-sched-bogus-d12858ad' is that run's scheduled-action name; it belongs in the entry's ScheduledActionName member, not appended to the message.

Model grounding (Smithy com.amazonaws.autoscaling 2011-01-01, awsQuery):
- DeleteLaunchConfiguration: input LaunchConfigurationName required; errors ResourceInUseFault (ResourceInUse/400) and ResourceContentionFault (500). No not-found error modeled -> service-common ValidationError; sentence recorded behavior only.
- DeleteScheduledAction: input AutoScalingGroupName + ScheduledActionName required; only ResourceContentionFault (500) modeled -> service-common ValidationError; sentence recorded behavior only.
- BatchDeleteScheduledAction: input AutoScalingGroupName + ScheduledActionNames required; only ResourceContentionFault (500) modeled; output BatchDeleteScheduledActionAnswer.FailedScheduledActions: list<FailedScheduledUpdateGroupActionRequest{ScheduledActionName required, ErrorCode, ErrorMessage}> — ErrorMessage documented as 'The error message accompanying the error code', value recorded behavior only.

## Steps to Reproduce
Build and boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.

Cause 1 (fixture autoscaling-delete-launch-configuration-never-created, step task#2):
1) autoscaling DeleteLaunchConfiguration(LaunchConfigurationName=<a name never created>). Expect HTTP 400, Error.Code 'ValidationError', Error.Message 'Launch configuration name not found - Launch configuration <that name> not found' — fixed prefix and fixed ' not found' suffix around the request's name, with no 'does not exist' prose.

Cause 2 (fixture autoscaling-policy-schedule-hook-notification-tag-lifecycle, step task#8):
2) autoscaling DeleteScheduledAction(AutoScalingGroupName=<a group that does not exist>, ScheduledActionName=<any name>). Expect HTTP 400, Error.Code 'ValidationError', Error.Message exactly 'AutoScalingGroup name not found - null' — the group name must not appear anywhere in the message.

Cause 3 (fixture autoscaling-policy-schedule-hook-notification-tag-lifecycle, step task#10):
3) autoscaling BatchDeleteScheduledAction(AutoScalingGroupName=<a group that does not exist>, ScheduledActionNames=[<any name>]). Expect HTTP 400, Error.Code 'ValidationError', Error.Message exactly 'AutoScalingGroup name not found - null' — a missing group fails the whole batch; it must not come back as a per-item FailedScheduledActions entry.

Cause 4 (fixture autoscaling-batch-delete-scheduled-action-partial-failure-envelope, step task#6):
4) Create a group, then PutScheduledUpdateGroupAction on it for one scheduled action <good>.
5) autoscaling BatchDeleteScheduledAction(AutoScalingGroupName=<that group>, ScheduledActionNames=[<good>, <a name that does not exist>]). Expect HTTP 200 with exactly one FailedScheduledActions entry whose ScheduledActionName is the missing name and whose ErrorMessage is exactly 'Scheduled action name not found' — no ' - ' clause and no repetition of the name. The <good> action must be deleted.
