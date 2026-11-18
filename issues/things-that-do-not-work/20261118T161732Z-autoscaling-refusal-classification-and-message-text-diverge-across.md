Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.complete-lifecycle-action-reused-on-a-consumed-action-rejected-and-the-instance-stays-waiting autoscaling.complete-lifecycle-action-unknown-hook-rejected autoscaling.describe-invalid-next-token-multi-endpoint-fails autoscaling.op-contract-attachInstances autoscaling.op-contract-enterStandby autoscaling.op-contract-createOrUpdateTags autoscaling.create-or-update-tags-key-and-value-length-boundary autoscaling.tag-resourcetype-other-than-auto-scaling-group-rejected autoscaling.launch-instances-idempotency-token-dedup autoscaling.create-launch-configuration-requires-instance-type-before-availability-refusal
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.complete-lifecycle-action-reused-on-a-consumed-action-rejected-and-the-instance-stays-waiting|run#17|AssertionError|assert message == (
Issue-Key: autoscaling.complete-lifecycle-action-unknown-hook-rejected|run#13|AssertionError|assert message == (
Issue-Key: autoscaling.describe-invalid-next-token-multi-endpoint-fails|run#7|AssertionError|assert err_instances.get("Message") == expected_instances, (
Issue-Key: autoscaling.op-contract-attachInstances|run#26|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: autoscaling.op-contract-enterStandby|run#13|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: autoscaling.op-contract-createOrUpdateTags|run#10|AssertionError|assert code == "LimitExceeded", f"expected LimitExceeded, got {code}"
Issue-Key: autoscaling.create-or-update-tags-key-and-value-length-boundary|run#17|AssertionError|assert message == (
Issue-Key: autoscaling.tag-resourcetype-other-than-auto-scaling-group-rejected|run#7|AssertionError|assert message == expected_message, (
Issue-Key: autoscaling.launch-instances-idempotency-token-dedup|run#24|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: autoscaling.create-launch-configuration-requires-instance-type-before-availability-refusal|expect_error#5|AssertionError|assert got_code == code, f"expected {code}, got {got_code}: {got_message!r}"
Issue-Commit: c9344e2779494e078eb181c7552bbea5ec625bd0

# autoscaling refusal classification and message text diverge across 9 error paths (lifecycle action, next token, attach/standby bounds, tags, launch config)

> _Note: this issue was identified against the code base at commit `c9344e2779494e078eb181c7552bbea5ec625bd0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CompleteLifecycleAction

## Description
Nine net-new causes in the autoscaling refusal plane. Every one answers HTTP 400; what diverges is the error Code the refusal is classified under, the fixed Message text AWS uses for that error class, or which of two checks runs first. Each cause must be closed separately; they are bundled only because they all live in one service's refusal plane.

Cause 1 of 9 — CompleteLifecycleAction / RecordLifecycleActionHeartbeat: the 'no active lifecycle action' refusal drops the HookName clause.
AWS: Smithy: CompleteLifecycleAction.LifecycleHookName (AsciiStringMaxLen255) and .AutoScalingGroupName are required, .LifecycleActionToken / .InstanceId optional; only ResourceContentionFault is modeled, so this ValidationError is recorded behavior only. For a token/instance that has no active action under the named hook (already consumed, or a hook name that does not exist), AWS answers 400 ValidationError with 'No active Lifecycle Action found with instance ID <instance-id> and HookName <hook-name>' — fixed text 'No active Lifecycle Action found with instance ID ' + the request's InstanceId + ' and HookName ' + the request's LifecycleHookName.
Emulator: 400 ValidationError 'No active Lifecycle Action found with instance ID <instance-id>' — the ' and HookName <hook-name>' half of the sentence is missing, so the refusal does not say which hook it looked under even though the hook name is the discriminator between a consumed action and a nonexistent hook.
Why: a client that matches AWS's sentence (or compares against a recorded golden) does not recognize the refusal, and an operator debugging a multi-hook group loses the only field that identifies which hook was checked.
Fix: the message template used by CompleteLifecycleAction's and RecordLifecycleActionHeartbeat's lifecycle-action lookup refusal.
Traces: autoscaling.complete-lifecycle-action-unknown-hook-rejected, autoscaling.complete-lifecycle-action-reused-on-a-consumed-action-rejected-and-the-instance-stays-waiting.

Cause 2 of 9 — DescribeAutoScalingInstances: the InvalidNextToken message uses the wrong tense.
AWS: Smithy: DescribeAutoScalingInstances models InvalidNextToken (errorKind client, awsQueryError code InvalidNextToken, httpResponseCode 400); NextToken targets XmlString. Recorded behavior: 400 with "The token '<token>' is invalid" — fixed text "The token '" + the request's NextToken + "' is invalid".
Emulator: 400 InvalidNextToken with "The token '<token>' was invalid" — correct code and variable part, wrong fixed text ('was' for 'is').
Why: pagination clients and golden comparisons that match AWS's sentence do not recognize the refusal.
Fix: the shared NextToken validation message template used by the autoscaling describe family. NOTE: open issue 20261117T053015Z-autoscaling-refusal-messages-and-validation-precedence-diverge-on.md records the same tense defect on DescribeScheduledActions (cause 5 there) from the wire corpus; it does not carry this check, and this check shows the same template is still wrong on DescribeAutoScalingInstances.
Traces: autoscaling.describe-invalid-next-token-multi-endpoint-fails.

Cause 3 of 9 — AttachInstances: the past-MaxSize refusal is emulator-authored.
AWS: Smithy: AttachInstances.AutoScalingGroupName required, .InstanceIds optional, no 400 modeled, so the bound refusal is recorded behavior only: 400 ValidationError with 'AutoScalingGroup <group> has min-size=<min>, max-size=<max>, and desired-size=<desired>. To attach <n> instance, please update the AutoScalingGroup sizes appropriately.' — fixed text with the group name, the group's three current sizes and the number of instances in the request interpolated (note the Oxford comma before 'and desired-size=').
Emulator: 400 with 'AutoScalingGroup <group> has min-size=<min>, max-size=<max>, desired-size=<desired>. New desired-size=<n> must be between min-size and max-size.' — the comma before 'and desired-size' is dropped and the second sentence is rewritten to talk about a new desired size rather than asking the caller to update the group sizes.
Why: a client matching AWS's sentence does not recognize the refusal, and the message no longer states the remediation AWS states.
Fix: AttachInstances' capacity-bound validation message. (Open issue 20261116T083932Z-autoscaling-attachinstances-all-three-validationerror-refusal-messages-are.md covers AttachInstances refusal texts from other checks but does not carry this one.)
Traces: autoscaling.op-contract-attachInstances.

Cause 4 of 9 — EnterStandby: the below-MinSize refusal is emulator-authored.
AWS: Smithy: EnterStandby.AutoScalingGroupName and .ShouldDecrementDesiredCapacity required; only ResourceContentionFault modeled, so this is recorded behavior only: 400 ValidationError with 'AutoScalingGroup <group> has min-size=<min>, max-size=<max>, and desired-size=<desired>. To place into standby <n> instance, please update the AutoScalingGroup sizes appropriately.' — same template family as cause 3, with 'To place into standby <n> instance'.
Emulator: 400 with "The desired capacity of the Auto Scaling group after the instances are placed into Standby (<n>) is below the minimum capacity of the group (<min>). Either set ShouldDecrementDesiredCapacity flag to false or lower group's min size." — entirely different prose.
Why: same as cause 3; two sibling operations that share one AWS template now share none.
Fix: EnterStandby's capacity-bound validation message.
Traces: autoscaling.op-contract-enterStandby.

Cause 5 of 9 — CreateOrUpdateTags: exceeding the per-resource tag limit is classified ValidationError instead of LimitExceeded.
AWS: Smithy: CreateOrUpdateTags models LimitExceededFault (errorKind client, awsQueryError code LimitExceeded, httpResponseCode 400). Recorded behavior for a call that would push a group past the 50-tag-per-resource limit: 400 with code LimitExceeded and the wholly fixed sentence 'You tried to create more tags than allowed' (no interpolated values), and no tag from the rejected call is stored.
Emulator: 400 with code ValidationError, so the modeled LimitExceededFault is never raised.
Why: Error.Code is what SDKs map to an exception class and what retry/branch logic keys on; a client catching LimitExceededFault to prune tags and retry never sees it and treats a quota refusal as a malformed request.
Fix: CreateOrUpdateTags' tag-count validation — raise the modeled LimitExceededFault with AWS's fixed sentence.
Traces: autoscaling.op-contract-createOrUpdateTags.

Cause 6 of 9 — CreateOrUpdateTags: the over-length constraint message truncates the offending value.
AWS: Smithy: Tag.Key -> TagKey @length 1..128, Tag.Value -> TagValue @length 0..256. Recorded behavior for a 257-character value: 400 ValidationError whose message is the standard constraint sentence "1 validation error detected: Value '<the full offending value>' at 'tags.<i>.member.value' failed to satisfy constraint: Member must have length less than or equal to 256" — the offending value appears IN FULL, and the member path and limit come from the violated constraint.
Emulator: the same sentence but with the value elided: "Value 'vvvv…vvv... [257 bytes]' at 'tags.1.member.value' …" — it substitutes an ellipsis plus a '[<n> bytes]' annotation of its own invention for the value AWS echoes in full.
Why: the echoed value is how a caller (and a golden comparison) identifies which of a batch of tags was rejected; an emulator-authored '... [257 bytes]' marker is text AWS never emits and breaks exact-message matching.
Fix: the shared constraint-violation message serializer for the autoscaling Query front-end — echo the offending value without truncation or byte annotation.
Traces: autoscaling.create-or-update-tags-key-and-value-length-boundary.

Cause 7 of 9 — CreateOrUpdateTags: the unsupported-ResourceType refusal is emulator-authored.
AWS: Smithy: Tag.ResourceType targets XmlString and is documented 'The only supported value is auto-scaling-group', so the check is server-side and its refusal is recorded behavior only: 400 ValidationError with 'Invalid resourceType: <resource-type>. Valid resource types are: auto-scaling-group ' — fixed text 'Invalid resourceType: ' + the request's ResourceType + '. Valid resource types are: auto-scaling-group ' (AWS's sentence ends with a trailing space and no period).
Emulator: 400 with 'Resource type <resource-type> is not correct. Allowed Resource types are: auto-scaling-group' — different fixed text in both halves.
Why: a client matching AWS's sentence does not recognize the refusal; the code alone does not distinguish it from CreateOrUpdateTags' other ValidationErrors, so the message is the discriminator.
Fix: CreateOrUpdateTags' ResourceType validation message.
Traces: autoscaling.tag-resourcetype-other-than-auto-scaling-group-rejected.

Cause 8 of 9 — LaunchInstances: the IdempotentParameterMismatch message is emulator-authored.
AWS: recorded behavior only for the message: reusing a ClientToken with different request parameters is refused 400 with code IdempotentParameterMismatch and the wholly fixed sentence 'Different request parameters found in the last request used with the provided token name' (no interpolated values), and the original launch is left untouched.
Emulator: 400 IdempotentParameterMismatch with 'The parameters in the current request do not match the parameters from a previous request with the same client token' — correct code, re-authored sentence.
Why: a client that matches AWS's sentence (or logs/goldens it) does not recognize the refusal.
Fix: the ClientToken replay check in LaunchInstances' handler — emit AWS's fixed sentence.
Traces: autoscaling.launch-instances-idempotency-token-dedup.

Cause 9 of 9 — CreateLaunchConfiguration: the account-availability refusal pre-empts the required-parameter check.
AWS: Smithy: CreateLaunchConfiguration.LaunchConfigurationName is required; ImageId, InstanceType and InstanceId are all optional in the model, so the 'InstanceId or (ImageId and InstanceType)' rule is a server-side validation. Recorded behavior: that argument-combination check runs FIRST — a request carrying ImageId but no InstanceType is refused 400 ValidationError with 'Valid requests must contain either the InstanceID parameter or both the ImageId and InstanceType parameters.', and only a well-formed request reaches the account-level refusal 400 UnsupportedOperation 'The Launch Configuration creation operation is not available in your account. Use launch templates to create configuration templates for your Auto Scaling groups.'
Emulator: the malformed request is answered with UnsupportedOperation and that launch-configuration-unavailable sentence, i.e. the availability gate is evaluated before the request's own parameters are validated.
Why: the caller is told the feature is unavailable when the request is also malformed, so validation ordering is observably different and a client that distinguishes 'fix your request' from 'use launch templates instead' branches wrongly.
Fix: CreateLaunchConfiguration's handler — order the ImageId/InstanceType/InstanceId presence validation ahead of the account-availability refusal.
Traces: autoscaling.create-launch-configuration-requires-instance-type-before-availability-refusal.

All group names, instance ids, hook names, token values, tag values and sizes quoted here are one run's request/state values filling the templates; only the fixed text, the error codes, the HTTP status and the templated positions are the requirement.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout (service heading '### autoscaling'); every call below answered HTTP 400 except where noted:
Cause 1 — autoscaling.complete-lifecycle-action-unknown-hook-rejected and autoscaling.complete-lifecycle-action-reused-on-a-consumed-action-rejected-and-the-instance-stays-waiting: expected Message 'No active Lifecycle Action found with instance ID i-00000000000000001 and HookName <hook>', got 'No active Lifecycle Action found with instance ID i-00000000000000001' (code ValidationError matched).
Cause 2 — autoscaling.describe-invalid-next-token-multi-endpoint-fails: DescribeAutoScalingInstances(NextToken='sim2real-not-a-real-next-token-xyz123'); code InvalidNextToken matched, Message expected "The token 'sim2real-not-a-real-next-token-xyz123' is invalid", got "The token 'sim2real-not-a-real-next-token-xyz123' was invalid".
Cause 3 — autoscaling.op-contract-attachInstances: expected 'AutoScalingGroup <group> has min-size=0, max-size=1, and desired-size=1. To attach 1 instance, please update the AutoScalingGroup sizes appropriately.', got 'AutoScalingGroup <group> has min-size=0, max-size=1, desired-size=1. New desired-size=2 must be between min-size and max-size.'
Cause 4 — autoscaling.op-contract-enterStandby: expected 'AutoScalingGroup <group> has min-size=1, max-size=1, and desired-size=1. To place into standby 1 instance, please update the AutoScalingGroup sizes appropriately.', got "The desired capacity of the Auto Scaling group after the instances are placed into Standby (0) is below the minimum capacity of the group (1). Either set ShouldDecrementDesiredCapacity flag to false or lower group's min size."
Cause 5 — autoscaling.op-contract-createOrUpdateTags: CreateOrUpdateTags with 55 tags on a group that already has 1; expected code LimitExceeded with 'You tried to create more tags than allowed', got code ValidationError.
Cause 6 — autoscaling.create-or-update-tags-key-and-value-length-boundary: a 257-character Value; expected "1 validation error detected: Value '<257 v characters>' at 'tags.1.member.value' failed to satisfy constraint: Member must have length less than or equal to 256", got the same sentence with the value replaced by 'vvvv…' + '... [257 bytes]'.
Cause 7 — autoscaling.tag-resourcetype-other-than-auto-scaling-group-rejected: ResourceType='autoscaling-group'; expected 'Invalid resourceType: autoscaling-group. Valid resource types are: auto-scaling-group ', got 'Resource type autoscaling-group is not correct. Allowed Resource types are: auto-scaling-group'.
Cause 8 — autoscaling.launch-instances-idempotency-token-dedup: LaunchInstances replayed with the same ClientToken and different parameters; code IdempotentParameterMismatch matched, Message expected 'Different request parameters found in the last request used with the provided token name', got 'The parameters in the current request do not match the parameters from a previous request with the same client token'.
Cause 9 — autoscaling.create-launch-configuration-requires-instance-type-before-availability-refusal: CreateLaunchConfiguration with ImageId and no InstanceType; expected code ValidationError 'Valid requests must contain either the InstanceID parameter or both the ImageId and InstanceType parameters.', got code UnsupportedOperation 'The Launch Configuration creation operation is not available in your account. Use launch templates to create configuration templates for your Auto Scaling groups.'

## Steps to Reproduce
1. Replay the behavioral suite (simparity-behavioral-aws-v1) against the emulator built from this checkout.
2. Cause 1: park an instance with a PutLifecycleHook launch hook, then call CompleteLifecycleAction (or RecordLifecycleActionHeartbeat) naming a hook that has no active action for that instance; read Error.Message.
3. Cause 2: DescribeAutoScalingInstances with a syntactically bogus NextToken; read Error.Message.
4. Cause 3: on a group with min=0/max=1/desired=1, AttachInstances a second instance; read Error.Message.
5. Cause 4: on a group with min=1/max=1/desired=1, EnterStandby one instance with ShouldDecrementDesiredCapacity=true; read Error.Message.
6. Cause 5: CreateOrUpdateTags with 55 tags for one group; read Error.Code and Error.Message.
7. Cause 6: CreateOrUpdateTags with a 257-character Value (and a 129-character Key); read Error.Message and compare the echoed value.
8. Cause 7: CreateOrUpdateTags with ResourceType='autoscaling-group'; read Error.Message.
9. Cause 8: LaunchInstances twice with one ClientToken and different parameters; read Error.Message.
10. Cause 9: CreateLaunchConfiguration with ImageId set and InstanceType/InstanceId absent; read Error.Code and Error.Message.
