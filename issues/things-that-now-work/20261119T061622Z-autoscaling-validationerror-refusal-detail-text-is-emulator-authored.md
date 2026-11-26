Issue-Source: trace-sweep-divergence
Issue-Trace: autoscaling-asgdx-traj-remove-instance-min-size-and-in-use-refusals
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: autoscaling-asgdx-traj-remove-instance-min-size-and-in-use-refusals autoscaling-group-configuration-and-topology-negative autoscaling-group-dependent-operations-on-nonexistent-group autoscaling-policy-schedule-hook-notification-tag-lifecycle autoscaling-scheduled-actions-on-nonexistent-group
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling|DetachInstances|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|ResumeProcesses|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|BatchPutScheduledUpdateGroupAction|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 744a403bcd89e11c6cc0112b0c3d4fd5ce52e365

# autoscaling: ValidationError refusal detail text is emulator-authored on three group-scoped operations

> _Note: this issue was identified against the code base at commit `744a403bcd89e11c6cc0112b0c3d4fd5ce52e365`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DetachInstances

## Description
Three net-new root causes with one shared fault: the Auto Scaling query-protocol refusals on these three operations answer the right HTTP status (400) and the right Error.Code (ValidationError), but Error.Message is text the emulator wrote itself rather than AWS's sentence for that error class. Each cause must be closed separately before this issue is submitted.

Cause 1 of 3 - DetachInstances' min-size refusal is re-worded and reports the wrong facts.
AWS: Smithy (com.amazonaws.autoscaling#AutoScaling_2011_01_01) models DetachInstances with input DetachInstancesQuery {AutoScalingGroupName required @length 1..255, InstanceIds, ShouldDecrementDesiredCapacity required} and binds only ResourceContentionFault (HTTP 500), so the size-constraint guard and its message are recorded behavior only. A DetachInstances whose ShouldDecrementDesiredCapacity would take the group below its MinSize is refused HTTP 400, Error.Code ValidationError, Error.Message "AutoScalingGroup <group-name> has min-size=<min>, max-size=<max>, and desired-size=<desired>. To detach <n> instance, please update the AutoScalingGroup sizes appropriately." The fixed text is "AutoScalingGroup ", " has min-size=", ", max-size=", ", and desired-size=", ". To detach ", " instance, please update the AutoScalingGroup sizes appropriately."; the variable parts are the request's AutoScalingGroupName, the group's stored MinSize/MaxSize/DesiredCapacity, and the number of ids in the request's InstanceIds. The refusal changes nothing: group membership and DesiredCapacity are unchanged.
Emulator: same status and code, but Error.Message is the emulator's own prose "Currently, desiredSize equals minSize (<n>). Detaching instance without replacement will violate group's min size constraint. Either set shouldDecrementDesiredCapacity flag to false or lower group's min size." - a different sentence shape that names neither the group nor its max-size and prescribes a remedy AWS does not word.
Why: the message is part of the refusal contract. A client (or console/CLI/support tooling) that matches AWS's sentence to tell this size guard apart from any other ValidationError on DetachInstances never recognizes the emulator's text, and it cannot parse the group's min/max/desired out of the refusal to decide how much to raise the sizes by.
Fix: the size-constraint validation in services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/ops/detach_instances.rs - emit AWS's sentence with the group name, the stored min/max/desired and the requested instance count interpolated.
Traces: autoscaling-asgdx-traj-remove-instance-min-size-and-in-use-refusals#task#7 (also autoscaling-group-configuration-and-topology-negative#task#8, autoscaling-group-dependent-operations-on-nonexistent-group#task#8).

Cause 2 of 3 - ResumeProcesses' group-not-found refusal drops the "no such group: <name>" detail.
AWS: Smithy models ResumeProcesses with input ScalingProcessQuery {AutoScalingGroupName required, ScalingProcesses} and binds ResourceContentionFault (500) and ResourceInUseFault (400); the not-found refusal is recorded behavior only. A ResumeProcesses naming a group that does not exist is refused HTTP 400, Error.Code ValidationError, Error.Message "AutoScalingGroup name not found - no such group: <group-name>": fixed text "AutoScalingGroup name not found - no such group: " followed by the AutoScalingGroupName taken from the request.
Emulator: same status and code, but Error.Message is "AutoScalingGroup name not found - null" - the detail clause after the dash is the literal string "null" instead of the "no such group: <name>" detail, so the refusal does not say which group was missing.
Why: AWS reuses the "AutoScalingGroup name not found - " prefix across many operations and distinguishes them by the detail clause; a client that logs or matches the refusal gets a message with no identifying detail, and a caller reconciling several group names cannot tell which name failed.
Fix: the group-lookup refusal in services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/ops/resume_processes.rs - emit the "no such group: <name>" detail from the request's AutoScalingGroupName instead of a null placeholder.
Traces: autoscaling-group-configuration-and-topology-negative#task#4 (also autoscaling-group-dependent-operations-on-nonexistent-group#task#6).

Cause 3 of 3 - BatchPutScheduledUpdateGroupAction's group-not-found refusal interpolates a detail AWS does not emit.
AWS: Smithy models BatchPutScheduledUpdateGroupAction with input {AutoScalingGroupName required, ScheduledUpdateGroupActions required} and binds AlreadyExistsFault, LimitExceededFault (both HTTP 400) and ResourceContentionFault (500); the not-found refusal is recorded behavior only. A batch naming a group that does not exist is refused HTTP 400, Error.Code ValidationError, Error.Message exactly "AutoScalingGroup name not found - null" - on this operation the whole message is fixed text, with no request-derived part; no scheduled action is created.
Emulator: same status and code, but Error.Message is "AutoScalingGroup name not found - AutoScalingGroup <group-name> not found", i.e. it substitutes its own detail clause (and repeats the resource kind) where AWS's text for this operation carries the literal "null" detail.
Why: this is the mirror image of cause 2 - the detail clause is per-operation, and the emulator has it inverted on both. A client matching AWS's exact body for the batch path (or comparing a golden error body) fails, and the two operations' refusals are no longer distinguishable in the way AWS distinguishes them.
Fix: the group-lookup refusal in services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/ops/batch_put_scheduled_update_group_action.rs - emit AWS's fixed sentence for this operation rather than an interpolated detail.
Traces: autoscaling-policy-schedule-hook-notification-tag-lifecycle#task#9 (also autoscaling-scheduled-actions-on-nonexistent-group#task#2).

All group names quoted in the evidence are one run's values filling these templates; only the fixed text and the request-/state-derived parts are requirements. Related but non-covering open issues (none of them carries these three replay keys): 20261118T161732Z-autoscaling-refusal-classification-and-message-text-diverge-across.md, 20261116T221312Z-autoscaling-refusal-messages-diverge-from-aws-s-fixed.md, 20261117T053015Z-autoscaling-refusal-messages-and-validation-precedence-diverge-on.md, and the behavioral-corpus issue 20261118T191234Z-autoscaling-4-net-new-causes-batchputscheduledupdategroupaction-never-reports.md (which concerns DetachInstances answering NotImplemented and the batch name-conflict report, not these message texts).

## Evidence
Replay of fixture corpus simparity-wire-aws-v1 against the emulator built from this checkout (68 divergence clusters; these three carried no owning issue and no stub).

Cause 1 - autoscaling-asgdx-traj-remove-instance-min-size-and-in-use-refusals#task#7, DetachInstances [mismatch]: status expected 400, got 400; $.ErrorResponse.Error.Message value_mismatch - expected 'AutoScalingGroup sim2real-probe-asgdx-1674bf44 has min-size=1, max-size=1, and desired-size=1. To detach 1 instance, please update the AutoScalingGroup sizes appropriately.', got "Currently, desiredSize equals minSize (1). Detaching instance without replacement will violate group's min size constraint. Either set shouldDecrementDesiredCapacity flag to false or lower group's min size.". Cluster also covers autoscaling-group-configuration-and-topology-negative#task#8 and autoscaling-group-dependent-operations-on-nonexistent-group#task#8 (3 steps).

Cause 2 - autoscaling-group-configuration-and-topology-negative#task#4, ResumeProcesses [mismatch]: status expected 400, got 400; $.ErrorResponse.Error.Message value_mismatch - expected 'AutoScalingGroup name not found - no such group: sim2real-probe-autoscaling-missing-asg', got 'AutoScalingGroup name not found - null'. Cluster also covers autoscaling-group-dependent-operations-on-nonexistent-group#task#6 (2 steps).

Cause 3 - autoscaling-policy-schedule-hook-notification-tag-lifecycle#task#9, BatchPutScheduledUpdateGroupAction [mismatch]: status expected 400, got 400; $.ErrorResponse.Error.Message value_mismatch - expected 'AutoScalingGroup name not found - null', got 'AutoScalingGroup name not found - AutoScalingGroup sim2real-probe-autoscaling-missing-asg not found'. Cluster also covers autoscaling-scheduled-actions-on-nonexistent-group#task#2 (2 steps).

In all three the HTTP status and Error.Code (ValidationError) already match; only Error.Message diverges.

## Steps to Reproduce
1. Replay the simparity-wire-aws-v1 wire corpus against the emulator built from this checkout.
2. Cause 1: inspect autoscaling-asgdx-traj-remove-instance-min-size-and-in-use-refusals#task#7 - a DetachInstances with ShouldDecrementDesiredCapacity=true against a group whose MinSize == DesiredCapacity; read Error.Message and compare with 'AutoScalingGroup <name> has min-size=<min>, max-size=<max>, and desired-size=<desired>. To detach <n> instance, please update the AutoScalingGroup sizes appropriately.'
3. Cause 2: inspect autoscaling-group-configuration-and-topology-negative#task#4 - a ResumeProcesses naming a group that does not exist; Error.Message must end with 'no such group: <the requested group name>'.
4. Cause 3: inspect autoscaling-policy-schedule-hook-notification-tag-lifecycle#task#9 - a BatchPutScheduledUpdateGroupAction naming a group that does not exist; Error.Message must be exactly 'AutoScalingGroup name not found - null'.
