Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.create-asg-invalid-launch-template-id-rejected autoscaling.describe-invalid-next-token-multi-endpoint-fails
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.create-asg-invalid-launch-template-id-rejected|run#9|AssertionError|assert message == f"Malformed launch template Id: {bogus_lt_id}", (
Issue-Key: autoscaling.describe-invalid-next-token-multi-endpoint-fails|run#12|AssertionError|assert err_lbs.get("Message") == no_max_records, (
Issue-Commit: a45eebcdd8d9a9d0a1f0906c2cbe25c9ebaa13ba

# autoscaling: 2 net-new refusal-message causes — CreateAutoScalingGroup re-words the malformed-LaunchTemplateId refusal, and NextToken-without-MaxRecords on DescribeLoadBalancers/DescribeTrafficSources echoes the token instead of AWS's sentence

> _Note: this issue was identified against the code base at commit `a45eebcdd8d9a9d0a1f0906c2cbe25c9ebaa13ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAutoScalingGroup

## Description
TWO net-new root causes in the Auto Scaling request-validation/refusal path. Both are refusals the emulator gets structurally right (HTTP 400 and the right Error.Code) and whose Error.Message is emulator-authored instead of AWS's template. This bundle carries 2 causes and BOTH must be closed before it is submitted.

Cause 1 of 2 — CreateAutoScalingGroup refuses a well-formed but never-minted LaunchTemplateId with the deleted-template sentence instead of AWS's malformed-id sentence.
AWS: HTTP 400, Error.Code 'ValidationError'. Smithy (services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json, awsQuery, version 2011-01-01) models CreateAutoScalingGroup errors as only AlreadyExistsFault (awsQueryError AlreadyExists/400), LimitExceededFault (LimitExceeded/400), ResourceContentionFault (500) and ServiceLinkedRoleFailure (500); no launch-template error is modeled, so the refusal rides the awsQuery service-common ValidationError and its sentence is recorded behavior only. Recorded behavior: a LaunchTemplate={'LaunchTemplateId': <id>} whose id is syntactically shaped like an lt- id but was never issued by this account/region is refused with Error.Message 'Malformed launch template Id: <launch-template-id>'. Message anatomy — fixed text: 'Malformed launch template Id: '; the single variable part is the LaunchTemplateId copied verbatim from the request's LaunchTemplateSpecification. No group is created, and the identical call against a live LaunchTemplateId succeeds. AWS thus has TWO distinct sentences on this path: this one for an id it cannot recognise at all, and 'The specified launch template, with template ID <id>, does not exist.' for a reference that is recognisable but resolves to nothing.
Emulator: HTTP 400 and Code 'ValidationError' (both correct), but Error.Message 'The specified launch template, with template ID <id>, does not exist.' — it collapses the two reference verdicts onto the does-not-exist sentence, so the malformed/unknown-id sentence is never served.
Why: ValidationError is Auto Scaling's generic client-error code, so the message is the only machine-visible explanation of what was wrong. A caller that distinguishes 'this id is not even a valid reference, stop and fix the template reference' from 'the template was there and is gone, re-create it' — the two drive different remediation, one a config fix and one a re-provision — is told the wrong one, and any test, log scraper or runbook keyed to AWS's wording treats a correctly refused create as an unrecognised failure.
Fix: the CreateAutoScalingGroup handler's launch-template resolution/validation step, in the ValidationError message it builds — it must keep AWS's two verdicts apart and emit the 'Malformed launch template Id: <id>' sentence for a LaunchTemplateId it cannot resolve to any template it ever issued, rather than reusing the deleted-template sentence. This is message construction in that operation's validation, not the awsQuery serializer. Related but not covering: open issue 20261115T105104Z-autoscaling-ec2-createautoscalinggroup-s-refusal-for-a-launch.md specifies the by-name/by-id does-not-exist sentences for a template that was created and then DELETED; it does not describe the malformed-id sentence and does not carry this check id. Open issue 20261102T114505Z-ec2-launch-template-read-validation-and-subnet-projection.md is about EC2-side template reads, not this refusal.
Traces: autoscaling.create-asg-invalid-launch-template-id-rejected.

Cause 2 of 2 — a NextToken sent without MaxRecords on the group-scoped paginated describes is refused with a message that just echoes the token.
AWS: HTTP 400, Error.Code 'ValidationError'. Smithy models DescribeLoadBalancers and DescribeTrafficSources each with input AutoScalingGroupName (required), NextToken (XmlString) and MaxRecords, errors InvalidNextToken (awsQueryError InvalidNextToken/400) and ResourceContentionFault (500), and paginated{inputToken=NextToken, outputToken=NextToken, pageSize=MaxRecords}. Recorded behavior only: when NextToken is supplied WITHOUT MaxRecords, these two operations refuse before the token is examined with the fixed sentence 'Cannot specify NextToken without MaxRecords' — the message has NO variable part (it does not quote the token, the group or the operation). The same malformed token on DescribeAutoScalingInstances, which has no such pairing rule, is instead refused with Code 'InvalidNextToken' and the message "The token '<token>' is invalid" (that one the emulator already serves correctly). The same calls without NextToken succeed on the fresh group.
Emulator: HTTP 400 and Code 'ValidationError' (both correct) but Error.Message 'NextToken: <token>' — a bare member-name-plus-value fragment with no sentence at all, which also leaks the rejected token back into the message where AWS's text is token-free.
Why: the caller is told nothing about the actual rule it broke (NextToken requires MaxRecords on these operations) and instead gets what looks like a field dump. A paginator that reads the message to decide whether to re-issue the page with a page size, and any assertion or operator runbook matching AWS's sentence, cannot act on it; worse, the per-operation distinction between the pairing ValidationError and the InvalidNextToken token verdict becomes invisible in logs.
Fix: the shared NextToken/MaxRecords validation used by the group-scoped paginated describe handlers (DescribeLoadBalancers, DescribeTrafficSources) — it must raise the ValidationError with AWS's fixed sentence instead of formatting '<member>: <value>'. Related but not covering: open issue 20261114T180324Z-autoscaling-group-scoped-not-found-refusals-re-word.md covers the ORDERING of the group lookup against NextToken validation and the group-not-found sentences; it does not describe the NextToken-without-MaxRecords sentence and does not carry this check id.
Traces: autoscaling.describe-invalid-next-token-multi-endpoint-fails.

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against this checkout (emulator serving, bootstrap_signal=false; 331/465 checks passed). Both checks below are listed under the report heading '### autoscaling' and are neither ALREADY FILED nor OWNED by an open issue.

1) [mismatch] autoscaling.create-asg-invalid-launch-template-id-rejected — AssertionError at the check's `assert message == f"Malformed launch template Id: {bogus_lt_id}"`:
   expected the exact nonexistent-LaunchTemplateId refusal message, got 'The specified launch template, with template ID lt-00000000000000000, does not exist.'
   The Code assertion (`ValidationError`) passed, so only the message diverges. Request: CreateAutoScalingGroup(AutoScalingGroupName=<new name>, LaunchTemplate={'LaunchTemplateId': 'lt-00000000000000000', 'Version': '$Latest'}, MinSize=0, MaxSize=1, AvailabilityZones=[<az>], VPCZoneIdentifier=<subnet>).
   Required message anatomy — fixed text 'Malformed launch template Id: ', variable part the request's LaunchTemplateId. The id 'lt-00000000000000000' is the check's chosen well-formed-but-unissued value; the requirement is the template, not that literal.
   The check's follow-on assertions (DescribeAutoScalingGroups == [] after the refusal, and the identical create against a freshly created template succeeding) are not reached, so only the message is known to diverge.

2) [mismatch] autoscaling.describe-invalid-next-token-multi-endpoint-fails — AssertionError at the check's `assert err_lbs.get("Message") == no_max_records`:
   DescribeLoadBalancers: expected message 'Cannot specify NextToken without MaxRecords', got 'NextToken: sim2real-not-a-real-next-token-xyz123'
   The earlier assertions passed: DescribeAutoScalingInstances(NextToken=<bogus>) correctly answered Code 'InvalidNextToken' with "The token '<token>' is invalid", and DescribeLoadBalancers' Code assertion ('ValidationError') passed — so the divergence is the message alone. The DescribeTrafficSources leg (same expected sentence) is not reached because the DescribeLoadBalancers assertion fires first; it rides the same validation and must serve the same sentence.
   Required message: the fixed sentence 'Cannot specify NextToken without MaxRecords' with no variable part. The token value in the replay is the check's constant and must NOT appear in the message.

Model grounding (Smithy com.amazonaws.autoscaling, 2011-01-01, services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json):
- CreateAutoScalingGroup: errors AlreadyExistsFault/LimitExceededFault (400) + ResourceContentionFault/ServiceLinkedRoleFailure (500); no launch-template error modeled -> service-common ValidationError, sentence is recorded behavior only.
- DescribeLoadBalancers / DescribeTrafficSources: AutoScalingGroupName required, NextToken + MaxRecords, errors InvalidNextToken (400) and ResourceContentionFault (500), paginated{inputToken=NextToken, outputToken=NextToken, pageSize=MaxRecords}; the NextToken-requires-MaxRecords rule and its sentence are recorded behavior only.
- DescribeAutoScalingInstances: same InvalidNextToken error but no required pairing with MaxRecords, which is why its verdict is InvalidNextToken rather than ValidationError.

## Steps to Reproduce
Build and boot the emulator from this checkout and replay suite simparity-behavioral-aws-v1.

Cause 1 (check autoscaling.create-asg-invalid-launch-template-id-rejected):
1) autoscaling CreateAutoScalingGroup(AutoScalingGroupName=<fresh name>, MinSize=0, MaxSize=1, LaunchTemplate={'LaunchTemplateId': 'lt-00000000000000000', 'Version': '$Latest'}, AvailabilityZones=[<an available AZ>], VPCZoneIdentifier=<a subnet in that AZ>). Expect HTTP 400, Error.Code 'ValidationError', Error.Message 'Malformed launch template Id: lt-00000000000000000' (fixed text + the request's id).
2) autoscaling DescribeAutoScalingGroups(AutoScalingGroupNames=[<that name>]) must return AutoScalingGroups == [].
3) ec2 CreateLaunchTemplate(LaunchTemplateName=<name>, LaunchTemplateData={ImageId:<AL2023 ami>, InstanceType:'t3.micro'}), then repeat step 1 with the returned LaunchTemplateId — it must succeed and DescribeAutoScalingGroups must return exactly that one group.

Cause 2 (check autoscaling.describe-invalid-next-token-multi-endpoint-fails):
4) Create a 0/1/0 group from a launch template (any AZ/subnet of the run's network).
5) autoscaling DescribeAutoScalingInstances(NextToken='sim2real-not-a-real-next-token-xyz123') — control, must stay HTTP 400 Code 'InvalidNextToken', Message "The token 'sim2real-not-a-real-next-token-xyz123' is invalid".
6) autoscaling DescribeLoadBalancers(AutoScalingGroupName=<group>, NextToken='sim2real-not-a-real-next-token-xyz123') with NO MaxRecords. Expect HTTP 400, Error.Code 'ValidationError', Error.Message exactly 'Cannot specify NextToken without MaxRecords'.
7) autoscaling DescribeTrafficSources(AutoScalingGroupName=<group>, NextToken=<same>) with NO MaxRecords. Expect the same 400 / 'ValidationError' / 'Cannot specify NextToken without MaxRecords'.
8) The same three calls without NextToken must succeed against the fresh group.
