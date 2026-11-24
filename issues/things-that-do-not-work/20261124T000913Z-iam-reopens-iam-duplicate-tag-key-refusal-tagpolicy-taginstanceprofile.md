Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam.tag-policy-duplicate-key-rejected iam.tag-instance-profile-duplicate-key-rejected
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam.tag-policy-duplicate-key-rejected|run#9|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: iam.tag-instance-profile-duplicate-key-rejected|run#11|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Commit: 496f13e8c3207ef947f41f94a4f2c11ee1d8339b

# REOPENS iam duplicate-tag-key refusal: TagPolicy/TagInstanceProfile now answer with the TagUser sentence instead of their own

> _Note: this issue was identified against the code base at commit `496f13e8c3207ef947f41f94a4f2c11ee1d8339b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagPolicy

## Description
REOPENS 20261117T135333Z-iam-5-net-new-causes-duplicate-tag-keys.md: its fix left the call refused.

That resolved issue's cause 1 was grounded in exactly these two checks (iam.tag-policy-duplicate-key-rejected, iam.tag-instance-profile-duplicate-key-rejected), where a Tags list repeating a key was ACCEPTED with HTTP 200. The duplicate list is now refused and the refusal class is right, but the refusal these two checks replay is still not AWS's: the message is the one AWS words for TagUser, not the one it words for TagPolicy / TagInstanceProfile, so both checks still fail on the same refusal. This is not reported as a new defect.

AWS: Smithy iam#TagPolicy -> input PolicyArn (required, arnType @length 20-2048) + Tags (required, tagListType @length 0-50); iam#TagInstanceProfile -> InstanceProfileName (required, instanceProfileNameType @length 1-128, @pattern ^[\w+=,.@-]+$) + Tags (required, tagListType @length 0-50). Both declare InvalidInputException (httpError 400, awsQueryError code InvalidInput). The model states no message text, so the text is recorded behavior only: for these two operations the duplicate-key refusal is HTTP 400 / Error.Code InvalidInput with the message `Duplicate tag keys found.` — one sentence, 100% FIXED text with nothing substituted from the request or from state (no key name, no policy ARN, no profile name, no count), and the stored tags are left untouched. Note that this sentence is operation-specific: the suite's iam.op-contract-tag-user check records AWS answering TagUser's duplicate-key refusal with the LONGER `Duplicate tag keys found. Please note that Tag keys are case insensitive.`, and the open issue 20261123T114935Z-iam-2-net-new-causes-simulateprincipalpolicy-reports-a.md (cause 2) asks for that longer sentence on TagUser. Both sentences are requirements; they belong to different operations.

Emulator: HTTP 400 and Error.Code InvalidInput are correct for both operations, but Error.Message is `Duplicate tag keys found. Please note that Tag keys are case insensitive.` — the TagUser sentence. The shared duplicate-key validation now emits one blanket message for every Tag* operation, so fixing TagUser's wording regressed TagPolicy's and TagInstanceProfile's.

Why: the refusal message is a requirement. A client, runbook or regression test that matches AWS's TagPolicy / TagInstanceProfile refusal byte-for-byte does not recognise this refusal, and a client that branches on the case-insensitivity clause to decide whether it may retry with a differently-cased key draws that conclusion for two operations where AWS never states it.

Fix: the duplicate-key rejection message in the shared IAM tag-input validation that the Tag* handlers call — the message must be chosen per operation (the one-sentence form for TagPolicy and TagInstanceProfile, the two-sentence form for TagUser) rather than being a single constant for all of them. Both directions must pass together: re-replay iam.op-contract-tag-user alongside the two checks below.

Traces: iam.tag-policy-duplicate-key-rejected, iam.tag-instance-profile-duplicate-key-rejected (and iam.op-contract-tag-user as the opposing requirement, currently passing).

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout: 925/1144 passed, 153 mismatched, 66 errored, bootstrap_signal false. Both checks below are reported blocked by no unimplemented stub and owned by no open issue.

### iam -> [mismatch] iam.tag-instance-profile-duplicate-key-rejected
  File check_iam_tag_instance_profile_duplicate_key_rejected.py, line 47, in run
    assert message == expected, f"expected message {expected!r}, got {message!r}"
  AssertionError: expected message 'Duplicate tag keys found.', got 'Duplicate tag keys found. Please note that Tag keys are case insensitive.'

### iam -> [mismatch] iam.tag-policy-duplicate-key-rejected
  File check_iam_tag_policy_duplicate_key_rejected.py, line 53, in run
    assert message == expected, f"expected message {expected!r}, got {message!r}"
  AssertionError: expected message 'Duplicate tag keys found.', got 'Duplicate tag keys found. Please note that Tag keys are case insensitive.'

In both checks the assertion on the error code ran before the message assertion and passed, so HTTP 400 / Error.Code InvalidInput are correct and only Error.Message diverges. The expected string is entirely fixed text — the extra sentence is not a per-run value.

Lineage: 20261117T135333Z-iam-5-net-new-causes-duplicate-tag-keys.md (fixed) cause 1 carried these two replay keys with the previous symptom (`expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, ...}}`).

Model grounding (services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json):
- TagPolicy: input PolicyArn (required, arnType, @length 20-2048), Tags (required, tagListType, @length 0-50); errors ConcurrentModification(409), InvalidInput(400), LimitExceeded(409), NoSuchEntity(404), ServiceFailure(500).
- TagInstanceProfile: input InstanceProfileName (required, @length 1-128, @pattern ^[\w+=,.@-]+$), Tags (required, tagListType, @length 0-50); same error set.
- Neither operation models any message text, so the sentence is recorded behavior only.

## Steps to Reproduce
1. git_clone the repo, build and boot the emulator from the checkout, then replay simparity-behavioral-aws-v1.
2. CreatePolicy a customer-managed policy, then TagPolicy(PolicyArn=<that policy>, Tags=[{Key:'k',Value:'a'},{Key:'k',Value:'b'}]).
3. CreateInstanceProfile, then TagInstanceProfile(InstanceProfileName=<that profile>, Tags=[{Key:'k',Value:'a'},{Key:'k',Value:'b'}]).
4. Both must answer HTTP 400 with Error.Code 'InvalidInput' and Error.Message exactly 'Duplicate tag keys found.' — one sentence. Observed: 'Duplicate tag keys found. Please note that Tag keys are case insensitive.'
5. ListPolicyTags / ListInstanceProfileTags and confirm the rejected call stored nothing.
6. TagUser(UserName=<user>, Tags=[{Key:'Team',Value:'a'},{Key:'Team',Value:'b'}]) must still answer the LONGER sentence 'Duplicate tag keys found. Please note that Tag keys are case insensitive.'
7. Re-replay iam.tag-policy-duplicate-key-rejected, iam.tag-instance-profile-duplicate-key-rejected and iam.op-contract-tag-user; all three must pass.
