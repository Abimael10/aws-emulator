Issue-Source: trace-sweep-divergence
Issue-Trace: autoscaling-group-configuration-and-topology-negative
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: autoscaling-group-configuration-and-topology-negative autoscaling-group-dependent-operations-on-nonexistent-group
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling|AttachInstances|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 02a4fcf1b47c41876a5d40faa140e945c54f7cde

# autoscaling AttachInstances group-not-found refusal emits 'null' instead of AWS's "AutoScalingGroup: <name> not found" clause

> _Note: this issue was identified against the code base at commit `02a4fcf1b47c41876a5d40faa140e945c54f7cde`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AttachInstances

## Description
One net-new root cause in the Auto Scaling instance-attach refusal path. This bundle carries 1 cause and it must be closed.

Cause 1 of 1: AttachInstances' group-not-found refusal never interpolates the requested AutoScalingGroupName — it serves the literal 'null' where AWS names the group.

AWS: Smithy (com.amazonaws.autoscaling#AttachInstances, 2011-01-01, services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json) models input AutoScalingGroupName (XmlStringMaxLen255, required, @length 1..255) plus InstanceIds, and models ONLY the server faults ResourceContentionFault (awsQueryError 'ResourceContention', httpError 500) and ServiceLinkedRoleFailure (awsQueryError 'ServiceLinkedRoleFailure', httpError 500). No not-found fault is modeled, so a request naming an Auto Scaling group that does not exist is refused with the awsQuery service-common ValidationError. Recorded behavior supplies the message template: HTTP 400, Error.Code 'ValidationError', Error.Message "AutoScalingGroup name not found - AutoScalingGroup: <group-name> not found". Fixed text of that template: the prefix 'AutoScalingGroup name not found - ', then 'AutoScalingGroup: ', then ' not found'. The single variable part is the request's AutoScalingGroupName, interpolated bare between 'AutoScalingGroup: ' and ' not found' (no quoting). Note this template is NOT the one AWS uses for the group-scoped describes ("... - Could not find Auto Scaling group named '<name>'"): AWS's not-found clause is per-operation, so a single emulator-wide template cannot be correct for all of them.

Emulator: HTTP 400 with Error.Code 'ValidationError' (correct) but Error.Message 'AutoScalingGroup name not found - null'. The status and code match; the emulator keeps AWS's leading clause and then substitutes the literal 'null' for the whole 'AutoScalingGroup: <name> not found' tail, so the requested group name is dropped entirely and every missing-group AttachInstances refusal reads identically regardless of which group was named.

Why: a client is told that some Auto Scaling group is missing but not which one, and is told it in a sentence AWS never emits. Against AWS the group name is recoverable from the refusal — which is the only way to disambiguate, since the code is the plain ValidationError shared by every Auto Scaling argument defect — so a caller that attaches instances to several groups in a batch, or that logs/re-raises the refusal, or that matches AWS's 'AutoScalingGroup: <name> not found' sentence to distinguish 'group gone' from any other ValidationError, gets nothing usable. The literal 'null' additionally reads as a serialization bug rather than a refusal, which misdirects anyone debugging against the emulator.

Fix: the AttachInstances handler's group-lookup refusal, in the ValidationError message it formats — the handler clearly resolves the name against state (it knows the group is absent), so only the message formatting drops it; interpolate the request's AutoScalingGroupName into AWS's per-operation template rather than emitting a shared/placeholder tail.

Traces: autoscaling-group-configuration-and-topology-negative#task#7 (cluster also covers autoscaling-group-dependent-operations-on-nonexistent-group#task#7). Related but NOT covering: open issue 20261114T180324Z-autoscaling-group-scoped-not-found-refusals-re-word.md, whose cause 2 is the DeleteNotificationConfiguration variant of the same 'null' defect and whose cause 1 is the group-scoped-describe template — neither carries this cluster's replay key and AttachInstances' template differs from both. Also related but not covering: 20261110T100847Z-autoscaling-refusal-fidelity-wrong-codes-and-emulator-authored.md (DetachLoadBalancerTargetGroups variant).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator (emulator_up=true, bootstrap_signal=false): 89 fixtures replayed, 1044 steps, 795 passed, score 76.1%, 63 divergence clusters. 62 of those clusters are already owned by open issues and 0 are stub-blocked; the cluster below is the only unowned one.

autoscaling-group-configuration-and-topology-negative#task#7 — AttachInstances, expected HTTP 400 / actual HTTP 400:
  $.ErrorResponse.Error.Message: value_mismatch — expected 'AutoScalingGroup name not found - AutoScalingGroup: sim2real-probe-autoscaling-missing-asg not found', got 'AutoScalingGroup name not found - null'.
  Cluster of 2 (affected_count=2): also autoscaling-group-dependent-operations-on-nonexistent-group#task#7.
  divergence_key: service=autoscaling|AttachInstances|status=400->400|keys=$.ErrorResponse.Error.Message

In the expected message, 'AutoScalingGroup name not found - ', 'AutoScalingGroup: ' and ' not found' are the fixed text; 'sim2real-probe-autoscaling-missing-asg' is the request's AutoScalingGroupName, a per-run probe name that is NOT part of the requirement — the requirement is that the request's group name appear in that position. Error.Code ('ValidationError') and HTTP status (400) already match and are not part of this finding.

Model evidence (Smithy com.amazonaws.autoscaling, 2011-01-01): AttachInstances input AutoScalingGroupName is XmlStringMaxLen255, clientOptional+required, @length 1..255; InstanceIds is a list; errors are ResourceContentionFault (500) and ServiceLinkedRoleFailure (500) only. No not-found error shape is modeled, so the group-not-found refusal is the awsQuery service-common ValidationError and its message text is recorded behavior only.

## Steps to Reproduce
Replay the bound wire corpus against the emulator (replay_wire_corpus) and inspect:
1. Fixture autoscaling-group-configuration-and-topology-negative, step task#7 — AttachInstances naming an AutoScalingGroupName that was never created (plus one or more InstanceIds). Compare $.ErrorResponse.Error.Message against AWS's 'AutoScalingGroup name not found - AutoScalingGroup: <requested-group-name> not found'; the emulator serves 'AutoScalingGroup name not found - null'.
2. Same check in fixture autoscaling-group-dependent-operations-on-nonexistent-group, step task#7 (the other member of this cluster).
3. Confirm HTTP 400 and Error.Code 'ValidationError' are unchanged — only the message tail diverges — and that re-running with a different nonexistent group name still yields the identical 'null' message, showing the name is never threaded into the template.
