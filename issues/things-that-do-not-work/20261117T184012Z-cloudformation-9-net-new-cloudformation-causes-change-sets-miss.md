Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.change-set-condition-flip-reports-added-or-removed-resource cloudformation.if-guarded-ref-follows-condition-flips-through-change-sets cloudformation.change-set-outputs-only-diff-is-executable-not-rejected-as-no-changes cloudformation.outputs-only-change-set-previews-no-resource-change cloudformation.create-stack-tags-over-fifty-rejected cloudformation.create-stack-unmodeled-capability-value-rejected cloudformation.deleted-stack-id-still-reports-delete-complete-after-name-reused cloudformation.export-blank-value-rejected-rolls-back-and-exports-nothing cloudformation.false-condition-skips-importvalue-of-missing-export cloudformation.select-index-past-the-list-end-fails-the-stack-rejected cloudformation.set-stack-policy-body-and-url-together-rejected
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.change-set-condition-flip-reports-added-or-removed-resource|_update#8|AssertionError|assert cs["Status"] == "CREATE_COMPLETE", f"change set: {cs['Status']!r} {cs.get('StatusReason')!r}"
Issue-Key: cloudformation.if-guarded-ref-follows-condition-flips-through-change-sets|_change_set#11|AssertionError|assert cs["Status"] == "CREATE_COMPLETE", f"change set {cs['Status']!r}: {cs.get('StatusReason')!r}"
Issue-Key: cloudformation.change-set-outputs-only-diff-is-executable-not-rejected-as-no-changes|run#25|AssertionError|assert out_cs["Status"] == "CREATE_COMPLETE", (
Issue-Key: cloudformation.outputs-only-change-set-previews-no-resource-change|run#12|AssertionError|assert previewed["Status"] == "CREATE_COMPLETE", (
Issue-Key: cloudformation.create-stack-tags-over-fifty-rejected|run#15|AssertionError|assert error.get("Message") == "Tags must have length less than or equal to 50", (
Issue-Key: cloudformation.create-stack-unmodeled-capability-value-rejected|run#11|AssertionError|assert error.get("Message") == (
Issue-Key: cloudformation|UpdateStack|ValidationError
Issue-Key: cloudformation.export-blank-value-rejected-rolls-back-and-exports-nothing|run#9|AssertionError|assert status == "ROLLBACK_COMPLETE", f"{key} export stack ended {status!r}"
Issue-Key: cloudformation.false-condition-skips-importvalue-of-missing-export|run#9|AssertionError|assert skip_status == "CREATE_COMPLETE", (
Issue-Key: cloudformation.select-index-past-the-list-end-fails-the-stack-rejected|_refusal#6|AssertionError|raise AssertionError(f"expected a refusal, but the call returned {answer!r}")
Issue-Key: cloudformation.set-stack-policy-body-and-url-together-rejected|run#15|AssertionError|assert error.get("Message") == "You cannot specify both StackPolicyURL and StackPolicyBody", (
Issue-Commit: 16dd3a69d4b4af3373dd95173ca2a60641ca57cc

# 9 net-new CloudFormation causes: change sets miss condition-only and outputs-only diffs, four refusal messages are mis-worded, Fn::Select/Fn::ImportValue/blank Export are mis-evaluated

> _Note: this issue was identified against the code base at commit `16dd3a69d4b4af3373dd95173ca2a60641ca57cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateChangeSet

## Description
Nine distinct CloudFormation control-plane defects. Each cause has its own failing check ids.

Cause 1 of 9: CreateChangeSet reports "no changes" when the only template difference is a condition flip that adds or removes a resource.
AWS: recorded behavior only. Flipping a parameter so a Conditions-guarded resource becomes (or stops being) present is a change: the change set reaches CREATE_COMPLETE and its Changes list carries an Add (or Remove) entry for that logical id.
Emulator: the change set settles at FAILED with StatusReason "The submitted information didn't contain changes. Submit different information to create a change set."
Why: condition-driven templates — the standard way one template serves several environments — cannot be previewed or deployed at all; the client is told there is nothing to do when a resource is about to be created or destroyed.
Fix: the CreateChangeSet differ (it must evaluate Conditions before diffing the resource set).
Traces: cloudformation.change-set-condition-flip-reports-added-or-removed-resource, cloudformation.if-guarded-ref-follows-condition-flips-through-change-sets

Cause 2 of 9: CreateChangeSet reports "no changes" when the only template difference is in the Outputs section.
AWS: recorded behavior only. An outputs-only diff is a real change: the change set reaches CREATE_COMPLETE, previews no resource change, and executing it updates the stack's outputs.
Emulator: the change set settles at FAILED with StatusReason "The submitted information didn't contain changes. Submit different information to create a change set."
Why: a client cannot add, remove or re-point a stack output (including an Export other stacks import) through a change set at all.
Fix: the CreateChangeSet differ (it must diff Outputs, not only Resources).
Traces: cloudformation.change-set-outputs-only-diff-is-executable-not-rejected-as-no-changes, cloudformation.outputs-only-change-set-previews-no-resource-change

Cause 3 of 9: the CreateStack refusal for more than 50 Tags carries a generic constraint message.
AWS: Smithy: cloudformation#Tags is a list with @length max 50. CreateStack with 51 tags is refused ValidationError with the message `Tags must have length less than or equal to 50` (entirely fixed text; the limit 50 is the modeled maximum).
Emulator: ValidationError with `1 validation error detected: Value '51 items' at 'tags' failed to satisfy constraint: Member must have length less than or equal to 50`.
Why: a client that matches the documented message to distinguish a tag-limit refusal from any other ValidationError misclassifies it.
Fix: the CreateStack input validation's message for the Tags length constraint.
Traces: cloudformation.create-stack-tags-over-fifty-rejected

Cause 4 of 9: the CreateStack refusal for an unmodeled Capabilities value lists the enum members in the wrong order.
AWS: Smithy: cloudformation#Capability is an enum of CAPABILITY_IAM, CAPABILITY_NAMED_IAM, CAPABILITY_AUTO_EXPAND. The refusal is ValidationError `1 validation error detected: Value '[<value>]' at 'capabilities' failed to satisfy constraint: Member must satisfy constraint: [Member must satisfy enum value set: [CAPABILITY_AUTO_EXPAND, CAPABILITY_NAMED_IAM, CAPABILITY_IAM]]` — the member list is rendered in that order, which is not the model's declaration order; variable part: the rejected value, taken from the request.
Emulator: the same message but with the list rendered `[CAPABILITY_IAM, CAPABILITY_NAMED_IAM, CAPABILITY_AUTO_EXPAND]`.
Why: a client matching the exact refusal message does not recognise it.
Fix: the CreateStack input validation's enum-constraint message rendering for Capabilities.
Traces: cloudformation.create-stack-unmodeled-capability-value-rejected

Cause 5 of 9: a mutating call addressed by a deleted stack's StackId is refused as nonexistent instead of as DELETE_COMPLETE.
AWS: recorded behavior only. A deleted stack stays addressable by its StackId, so a mutating call on it is refused ValidationError `Stack:<stackId> is in DELETE_COMPLETE state and can not be updated.` Fixed text: the `Stack:` prefix (no space), the state name and ` state and can not be updated.`; variable part: the stack's ARN from the request.
Emulator: ValidationError `Stack [<stackId>] does not exist` — a different wording and a different meaning (the id is unknown rather than deleted), which also contradicts the DescribeStacks answer for the same id.
Why: a client that reuses a stack name and keeps the old StackId to tell "this stack was deleted" from "this id was never a stack" cannot do so; retry logic keyed on the DELETE_COMPLETE wording takes the wrong branch.
Fix: the stack lookup used by the mutating operations (a deleted stack must still resolve by StackId) and that path's refusal message.
Traces: cloudformation.deleted-stack-id-still-reports-delete-complete-after-name-reused

Cause 6 of 9: an Output whose Export Value resolves to the empty string is accepted.
AWS: recorded behavior only. A blank exported value is refused during provisioning, so the stack settles at ROLLBACK_COMPLETE and no export is listed under that name.
Emulator: the stack reaches CREATE_COMPLETE, so a blank export is published.
Why: downstream stacks import an empty value instead of failing fast, and the export name is consumed; the error surfaces far from its cause.
Fix: the stack's output/export resolution (reject an empty exported value and fail the create).
Traces: cloudformation.export-blank-value-rejected-rolls-back-and-exports-nothing

Cause 7 of 9: an Fn::ImportValue inside an unselected Fn::If branch is still resolved.
AWS: recorded behavior only. Only the selected branch of an Fn::If is evaluated, so a template whose unselected branch imports an export that does not exist reaches CREATE_COMPLETE.
Emulator: the stack settles at ROLLBACK_COMPLETE — the unselected branch's import is resolved and its failure fails the stack.
Why: the standard pattern of guarding an optional cross-stack import behind a condition is unusable; a template AWS deploys is refused here.
Fix: the template intrinsic evaluator (Fn::If must not evaluate the branch it does not select).
Traces: cloudformation.false-condition-skips-importvalue-of-missing-export

Cause 8 of 9: an Fn::Select index past the end of its list is accepted by CreateStack.
AWS: recorded behavior only. The template passes ValidateTemplate but CreateStack is refused ValidationError `Template error: Fn::Select  cannot select nonexistent value at index <index>` (fixed text, including the two spaces after `Fn::Select`; variable part: the index from the template), and no stack is created under that name.
Emulator: CreateStack answers 200 with a StackId, so a stack is created for a template AWS refuses.
Why: a client gets a stack it must now clean up instead of a synchronous refusal, and the out-of-range selection is resolved to something AWS never produces.
Fix: the Fn::Select intrinsic evaluation in the pre-provisioning CreateStack validation.
Traces: cloudformation.select-index-past-the-list-end-fails-the-stack-rejected

Cause 9 of 9: the SetStackPolicy mutually-exclusive-parameter refusal names the parameters in the wrong order.
AWS: recorded behavior only. Passing StackPolicyBody and StackPolicyURL together is refused ValidationError `You cannot specify both StackPolicyURL and StackPolicyBody` (entirely fixed text).
Emulator: ValidationError `You cannot specify both StackPolicyBody and StackPolicyURL`.
Why: a client matching the exact refusal message does not recognise it.
Fix: the SetStackPolicy input validation's refusal message.
Traces: cloudformation.set-stack-policy-body-and-url-together-rejected

## Evidence
Cause 1: "change set: 'FAILED' \"The submitted information didn't contain changes. Submit different information to create a change set.\"" where CREATE_COMPLETE with a single Add/Remove change was expected.
Cause 2: "outputs-only change set: 'FAILED' \"The submitted information didn't contain changes...\"" and "the outputs-only change set settled at 'FAILED', expected CREATE_COMPLETE".
Cause 3: got {'Code': 'ValidationError', 'Message': "1 validation error detected: Value '51 items' at 'tags' failed to satisfy constraint: Member must have length less than or equal to 50"}; expected Message "Tags must have length less than or equal to 50".
Cause 4: got Message "...enum value set: [CAPABILITY_IAM, CAPABILITY_NAMED_IAM, CAPABILITY_AUTO_EXPAND]]"; expected "...enum value set: [CAPABILITY_AUTO_EXPAND, CAPABILITY_NAMED_IAM, CAPABILITY_IAM]]".
Cause 5: expected ('ValidationError', 'Stack:<arn> is in DELETE_COMPLETE state and can not be updated.'), got ('ValidationError', 'Stack [<arn>] does not exist').
Cause 6: "empty export stack ended 'CREATE_COMPLETE'" (expected ROLLBACK_COMPLETE).
Cause 7: "the stack whose import branch is unselected must reach CREATE_COMPLETE, got 'ROLLBACK_COMPLETE'".
Cause 8: "expected a refusal, but the call returned {'StackId': 'arn:aws:cloudformation:...', 'OperationId': ..., 'HTTPStatusCode': 200}"; expected ('ValidationError', 'Template error: Fn::Select  cannot select nonexistent value at index <index>').
Cause 9: got Message 'You cannot specify both StackPolicyBody and StackPolicyURL'; expected 'You cannot specify both StackPolicyURL and StackPolicyBody'.

## Steps to Reproduce
Replay the behavioral suite and re-run exactly these checks:
1. cloudformation.change-set-condition-flip-reports-added-or-removed-resource
2. cloudformation.if-guarded-ref-follows-condition-flips-through-change-sets
3. cloudformation.change-set-outputs-only-diff-is-executable-not-rejected-as-no-changes
4. cloudformation.outputs-only-change-set-previews-no-resource-change
5. cloudformation.create-stack-tags-over-fifty-rejected
6. cloudformation.create-stack-unmodeled-capability-value-rejected
7. cloudformation.deleted-stack-id-still-reports-delete-complete-after-name-reused
8. cloudformation.export-blank-value-rejected-rolls-back-and-exports-nothing
9. cloudformation.false-condition-skips-importvalue-of-missing-export
10. cloudformation.select-index-past-the-list-end-fails-the-stack-rejected
11. cloudformation.set-stack-policy-body-and-url-together-rejected
