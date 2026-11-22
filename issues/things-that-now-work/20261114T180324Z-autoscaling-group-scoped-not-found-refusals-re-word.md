Issue-Source: trace-sweep-divergence
Issue-Trace: autoscaling-group-dependent-operations-on-nonexistent-group
Issue-Step: task#14
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: autoscaling-group-dependent-operations-on-nonexistent-group autoscaling-group-scoped-describes-on-missing-group autoscaling-notification-configuration-on-nonexistent-group autoscaling-policy-schedule-hook-notification-tag-lifecycle autoscaling-pagination-invalid-next-token-errors
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling|DescribeLoadBalancerTargetGroups|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|DeleteNotificationConfiguration|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|DescribeLoadBalancerTargetGroups|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Commit: 925fe565299dec6eb0f742dd224c6e41f11f8969

# autoscaling group-scoped not-found refusals re-word AWS text and lose precedence over NextToken validation

> _Note: this issue was identified against the code base at commit `925fe565299dec6eb0f742dd224c6e41f11f8969`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeLoadBalancerTargetGroups

## Description
Three net-new root causes in the Auto Scaling group-scoped refusal path. All three concern what the emulator serves when a request names an Auto Scaling group that does not exist: the emulator does refuse with HTTP 400, but it substitutes a sentence of its own for AWS's per-operation message template, and in one case it lets NextToken validation win over the group lookup so the caller gets a different modeled error code entirely. This bundle carries 3 causes and all 3 must be closed.

Cause 1 of 3: DescribeLoadBalancerTargetGroups' (and the other group-scoped describes') group-not-found message replaces AWS's "Could not find Auto Scaling group named '<name>'" clause with an emulator-authored sentence.
AWS: Smithy (com.amazonaws.autoscaling#DescribeLoadBalancerTargetGroups, 2011-01-01) models only InvalidNextToken (awsQueryError 'InvalidNextToken', 400) and ResourceContentionFault (500), so an unknown AutoScalingGroupName is refused with the awsQuery service-common ValidationError. Recorded behavior only: HTTP 400, Code 'ValidationError', Message "AutoScalingGroup name not found - Could not find Auto Scaling group named '<group-name>'". Fixed text of that template: 'AutoScalingGroup name not found - ' + "Could not find Auto Scaling group named '" + "'"; the single variable part is the AutoScalingGroupName taken verbatim from the request, single-quoted.
Emulator: HTTP 400, Code 'ValidationError', Message 'AutoScalingGroup name not found - AutoScalingGroup <group-name> not found' — it keeps AWS's leading clause and then appends its own generic 'AutoScalingGroup <name> not found' sentence instead of AWS's 'Could not find Auto Scaling group named ...' phrasing (no quoting of the name).
Why: the refusal message is part of the API contract. A client or test that matches AWS's sentence for a missing group — the common idiom for distinguishing 'group gone' from any other ValidationError, since the code is the same for all of them — never matches, and a human reading the refusal sees text AWS never emits. Note also that AWS's not-found clause is NOT uniform across Auto Scaling operations (see cause 2, and DetachLoadBalancerTargetGroups which emits the literal 'null'), so a single emulator-wide template cannot be right for all of them.
Emulator fix: the group-lookup refusal shared by the group-scoped describe handlers (DescribeLoadBalancerTargetGroups and the group-scoped describes replayed in the same cluster), in the ValidationError message it formats — the per-operation message template must be reproduced, not a single generic one.
Traces: autoscaling-group-dependent-operations-on-nonexistent-group#task#14 (cluster also covers autoscaling-group-scoped-describes-on-missing-group#task#2). Related but not covering: open issue 20261110T100847Z-autoscaling-refusal-fidelity-wrong-codes-and-emulator-authored.md, whose cause 6 is the DetachLoadBalancerTargetGroups variant ('... - null'); it does not carry this cluster's replay key and its template is a different one.

Cause 2 of 3: DeleteNotificationConfiguration's group-not-found message emits 'null' instead of the requested group name.
AWS: Smithy models only ResourceContentionFault (500) on DeleteNotificationConfiguration, with AutoScalingGroupName required, so an unknown group is refused with the service-common ValidationError. Recorded behavior only: HTTP 400, Code 'ValidationError', Message 'AutoScalingGroup name not found - <group-name>'. Fixed text: 'AutoScalingGroup name not found - '; the variable part is the AutoScalingGroupName from the request, appended bare (no quoting, no trailing sentence).
Emulator: HTTP 400 with Message 'AutoScalingGroup name not found - null' — the requested name is never threaded into the message, so every missing-group refusal for this operation (and the notification-configuration operations clustered with it) reads identically and names no group.
Why: the caller is told a group is missing but not which one. Against AWS the name is recoverable from the message; against the emulator it is lost, so a client that logs or re-raises the refusal, or that disambiguates a batch of per-group deletes by the name in the message, cannot tell which group failed. The literal 'null' also looks like a serialization bug rather than a refusal.
Emulator fix: the DeleteNotificationConfiguration handler's group-lookup refusal — the ValidationError message must interpolate the request's AutoScalingGroupName after the dash (its state lookup clearly has the name; only the message formatting drops it).
Traces: autoscaling-notification-configuration-on-nonexistent-group#task#2 (cluster also covers autoscaling-policy-schedule-hook-notification-tag-lifecycle#task#14). Related but not covering: open issue 20261112T031353Z-autoscaling-lifecycle-hook-and-notification-configuration-operations-diverge.md — it does not carry this cluster's replay key.

Cause 3 of 3: NextToken validation runs before the group lookup, so a paginated describe on a missing group is refused with InvalidNextToken instead of ValidationError.
AWS: for a group-scoped paginated describe, the group's existence is validated first: a request naming a nonexistent AutoScalingGroupName AND a malformed NextToken is refused HTTP 400, Code 'ValidationError', Message "AutoScalingGroup name not found - Could not find Auto Scaling group named '<group-name>'" — the token is never examined because the group it would page over does not exist. Smithy: DescribeLoadBalancerTargetGroups models InvalidNextToken (awsQueryError 'InvalidNextToken', httpError 400) and paginated{inputToken=NextToken,outputToken=NextToken,pageSize=MaxRecords}; the group-not-found refusal is the service-common ValidationError. Recorded behavior supplies the ordering.
Emulator: HTTP 400, Code 'InvalidNextToken', Message "The token '<token>' was invalid" — it validates the pagination token first and never reaches the group lookup, so it reports a token defect for a request whose actual defect is the missing group.
Why: the two codes steer a client in opposite directions. InvalidNextToken means 'drop your cursor and restart the page walk against this group', which against AWS would be an endless retry because the group does not exist; ValidationError means 'the group is gone, stop paging'. Any paginator that special-cases InvalidNextToken by restarting from no token loops, and error-classification logic that treats InvalidNextToken as retryable and ValidationError as terminal gets the retryability backwards.
Emulator fix: the validation ordering in the group-scoped paginated describe handlers — resolve AutoScalingGroupName against state and raise the ValidationError not-found refusal before the NextToken is decoded/validated.
Traces: autoscaling-pagination-invalid-next-token-errors#task#10.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator (emulator_up=true, bootstrap_signal=false): 89 fixtures replayed, 1044 steps, 720 passed, score 69.0%, 80 divergence clusters. 77 clusters are already owned by open issues and 0 are stub-blocked; the three clusters below are the unowned ones, all service=autoscaling, all HTTP 400 vs HTTP 400.

1) autoscaling-group-dependent-operations-on-nonexistent-group#task#14 — DescribeLoadBalancerTargetGroups, expected 400 / actual 400:
   $.ErrorResponse.Error.Message: value_mismatch — expected "AutoScalingGroup name not found - Could not find Auto Scaling group named 'sim2real-probe-autoscaling-no-such-asg-f07a1689'", got 'AutoScalingGroup name not found - AutoScalingGroup sim2real-probe-autoscaling-no-such-asg-f07a1689 not found'.
   Cluster of 2: also autoscaling-group-scoped-describes-on-missing-group#task#2.
   (The group name is the request's AutoScalingGroupName — a per-run probe name, not part of the requirement; the requirement is the template around it.)

2) autoscaling-notification-configuration-on-nonexistent-group#task#2 — DeleteNotificationConfiguration, expected 400 / actual 400:
   $.ErrorResponse.Error.Message: value_mismatch — expected 'AutoScalingGroup name not found - sim2real-probe-autoscaling-asg-bb5608a0', got 'AutoScalingGroup name not found - null'.
   Cluster of 2: also autoscaling-policy-schedule-hook-notification-tag-lifecycle#task#14.

3) autoscaling-pagination-invalid-next-token-errors#task#10 — DescribeLoadBalancerTargetGroups, expected 400 / actual 400:
   $.ErrorResponse.Error.Code: value_mismatch — expected 'ValidationError', got 'InvalidNextToken';
   $.ErrorResponse.Error.Message: value_mismatch — expected "AutoScalingGroup name not found - Could not find Auto Scaling group named 'sim2real-probe-autoscaling-missing-asg'", got "The token 'sim2real-probe-invalid-next-token' was invalid".

Model evidence (Smithy com.amazonaws.autoscaling, 2011-01-01, services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json):
- DescribeLoadBalancerTargetGroups: input AutoScalingGroupName (XmlStringMaxLen255, required, length 1..255), NextToken, MaxRecords; errors InvalidNextToken (awsQueryError 'InvalidNextToken', httpError 400) and ResourceContentionFault (500); paginated{inputToken=NextToken, outputToken=NextToken, pageSize=MaxRecords}. No not-found fault is modeled, so group-not-found is the awsQuery service-common ValidationError.
- DeleteNotificationConfiguration: input AutoScalingGroupName and TopicARN (both required); errors ResourceContentionFault (500) only — group-not-found is likewise the common ValidationError.
The message templates and the group-lookup-before-token ordering are recorded behavior only.

## Steps to Reproduce
Replay the bound wire corpus against the emulator (replay_wire_corpus) and inspect these steps:
1. Fixture autoscaling-group-dependent-operations-on-nonexistent-group, step task#14 — DescribeLoadBalancerTargetGroups with an AutoScalingGroupName that was never created; compare $.ErrorResponse.Error.Message against AWS's "AutoScalingGroup name not found - Could not find Auto Scaling group named '<name>'". Same check in autoscaling-group-scoped-describes-on-missing-group#task#2.
2. Fixture autoscaling-notification-configuration-on-nonexistent-group, step task#2 — DeleteNotificationConfiguration naming a nonexistent AutoScalingGroupName plus a TopicARN; compare $.ErrorResponse.Error.Message, which must end with the requested group name and not the literal 'null'. Same check in autoscaling-policy-schedule-hook-notification-tag-lifecycle#task#14.
3. Fixture autoscaling-pagination-invalid-next-token-errors, step task#10 — DescribeLoadBalancerTargetGroups with BOTH a nonexistent AutoScalingGroupName and a malformed NextToken; compare $.ErrorResponse.Error.Code (must be 'ValidationError', not 'InvalidNextToken') and $.ErrorResponse.Error.Message (must be the group-not-found sentence, not the invalid-token sentence).
