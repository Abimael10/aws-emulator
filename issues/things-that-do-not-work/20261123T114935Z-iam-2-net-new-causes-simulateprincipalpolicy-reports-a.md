Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam.sep-policy-composition-stringnotequals-missing-sourcevpce-key-deny-applies-denied-by-simulator-and-real-call iam.op-contract-tag-user
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam.sep-policy-composition-stringnotequals-missing-sourcevpce-key-deny-applies-denied-by-simulator-and-real-call|run#15|AssertionError|assert missing == [], (
Issue-Key: iam.op-contract-tag-user|run#11|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Commit: d4a73420c47c8214eb7a45d67f6bbcf3a13465a4

# iam: 2 net-new causes — SimulatePrincipalPolicy reports a condition key absent under a NEGATED operator as a missing context value, and the duplicate-tag-key refusal message is truncated

> _Note: this issue was identified against the code base at commit `d4a73420c47c8214eb7a45d67f6bbcf3a13465a4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SimulatePrincipalPolicy

## Description
Two net-new root causes in the iam service, both grounded in their own failing behavioral check. BOTH must be closed before this issue is submitted.

Cause 1 of 2: SimulatePrincipalPolicy lists a condition key that is ABSENT from the request as a MissingContextValue even when a negated condition operator has already DECIDED the condition.
AWS: Smithy iam#SimulatePrincipalPolicy -> output SimulatePolicyResponse.EvaluationResults (EvaluationResultsListType); EvaluationResult.MissingContextValues -> ContextKeyNamesResultListType, documented as "A list of context keys that are required by the included input policies but that were not provided by one of the input parameters" and, for the top-level result, "the deduplicated set of missing context values across all requested resources". AWS's condition-operator rules (IAM User Guide, reference_policies_elements_condition_operators) make the negated matching operators self-deciding on an absent key: a Deny statement conditioned on {"StringNotEquals": {"aws:SourceVpce": "<vpce-id>"}} evaluated for a request that carries NO aws:SourceVpce key matches — the condition is TRUE — so the statement applies and the authorization decision is fully determined. The key is therefore not "required but not provided" in the sense that leaves the simulation undecided: AWS answers HTTP 200 with EvalDecision 'explicitDeny' for the guarded action and an EMPTY MissingContextValues (top level and per-resource), and the real signed call is AccessDenied. Supplying the key with the matching value flips the same condition to false and the decision to 'allowed'; swapping only the operator to the positive StringEquals form on the same absent key evaluates false and the Deny stops applying.
Emulator: the decision itself is right — EvalDecision is 'explicitDeny' (the settle on {iam:GetUser: allowed, iam:ListUserTags: explicitDeny} passed) — but the same EvaluationResult carries MissingContextValues = ['aws:SourceVpce']. The emulator collects every condition key referenced by the policy that the request did not supply, without first asking whether the operator that references it is a negated one that the absence itself decides.
Why: MissingContextValues is how a caller learns its answer is CONDITIONAL. A pre-flight permission checker, a remediation loop, or a policy linter that reads a non-empty MissingContextValues treats the verdict as inconclusive and either re-simulates with extra ContextEntries forever or suppresses a deny AWS has definitively returned — while the real call is refused AccessDenied. The emulator thus reports "I cannot decide" for the one evaluation class (an absent key under a negated operator) where AWS is certain.
Fix: the condition-key evaluation in the policy-simulation engine behind SimulatePrincipalPolicy (and the SimulateCustomPolicy path that shares it) in the iam service crate — a context key referenced only by a negated matching operator (StringNotEquals/ArnNotEquals/NumericNotEquals/... and their IfExists forms) must be dropped from the missing-context collection once its absence has decided the condition; only keys whose absence leaves a statement undecided belong in MissingContextValues (top level) or in the matching ResourceSpecificResults entry when ResourceArns was supplied.
Related open issues, none of which carries this check's replay key: 20261116T105641Z-iam-7-net-new-causes-minted-access-key.md (cause 5 — missing keys NOT reported where AWS reports them, the opposite direction), 20261119T062349Z-cloudformation-ecr-iam-s3-ssm-simulateprincipalpolicy-reports-implicitdeny.md, 20261029T192356Z-iam-9-net-new-authorization-and-policy-evaluation.md.
Traces: iam.sep-policy-composition-stringnotequals-missing-sourcevpce-key-deny-applies-denied-by-simulator-and-real-call.

Cause 2 of 2: the Tag* duplicate-tag-key refusal message is a truncated version of AWS's sentence — it drops the case-insensitivity clause.
AWS: Smithy iam#TagUser -> input UserName (required, existingUserNameType) + Tags (required, tagListType, @length 0-50); errors include InvalidInputException, awsQueryError code 'InvalidInput', httpResponseCode 400. The model does not state the message text, so the text is recorded behavior only: a TagUser call whose Tags list repeats a key is refused HTTP 400 / Error.Code 'InvalidInput' with the message 'Duplicate tag keys found. Please note that Tag keys are case insensitive.' — the sentence is 100% FIXED text, with no part substituted from the request or from state (no key name, no user name, no count), so the whole two-sentence string including the trailing period is the requirement. The stored tags are left untouched by the refusal.
Emulator: HTTP 400 with Error.Code 'InvalidInput' (both correct) but Message 'Duplicate tag keys found.' — AWS's first sentence only; the second sentence, which is the part that tells the client the comparison is case-insensitive, is missing.
Why: the refusal message is a requirement, and this one carries information the code does not: a client that retries with 'Team' and 'team' believes it has distinct keys, because the emulator never says the keys are compared case-insensitively. Clients and runbooks that match or display the documented sentence do not recognise the refusal at all.
Fix: the duplicate-key rejection message in the shared IAM tag-input validation used by the Tag* operations (TagUser/TagRole/TagPolicy/TagInstanceProfile/...), which the fix for the resolved issue 20261117T135333Z-iam-5-net-new-causes-duplicate-tag-keys.md introduced — emit AWS's full sentence, not its first clause. That resolved issue's cause 1 was the duplicate list being ACCEPTED; the call is now correctly refused with the correct code, so this is NOT a reopen of it — only the message text remains wrong.
Traces: iam.op-contract-tag-user.

## Evidence
Behavioral replay of simparity-behavioral-aws-v1 against the emulator built from this checkout: 919/1144 passed, 158 mismatched, 67 errored. Both clusters below are reported owned by no open issue and blocked by no unimplemented stub.

Cause 1 — iam.sep-policy-composition-stringnotequals-missing-sourcevpce-key-deny-applies-denied-by-simulator-and-real-call [mismatch]:
  File check_iam_sep_policy_composition_stringnotequals_missing_sourcevpce_key_deny_applies_denied_by_simulator_and_real_call.py, line 170, in run
    assert missing == [], ...
  AssertionError: an absent key under a negated operator is DECIDED (condition true), not undecidable, so the simulator must not report it missing; got ['aws:SourceVpce']
The preceding poll on EvalDecision ({'iam:GetUser': 'allowed', 'iam:ListUserTags': 'explicitDeny'}) succeeded, so the emulator returned HTTP 200 with the CORRECT decision and still listed aws:SourceVpce in EvaluationResults[...].MissingContextValues.
Expected (AWS): HTTP 200, EvalDecision 'explicitDeny' for iam:ListUserTags, MissingContextValues == [] (top level and per-resource); with ContextEntries supplying aws:SourceVpce = <the vpce id in the Deny> the decision becomes 'allowed'; after swapping the operator to the positive StringEquals on the same absent key the decision becomes 'allowed' and the real ListUserTags call succeeds.

Cause 2 — iam.op-contract-tag-user [mismatch]:
  File check_iam_op_contract_tag_user.py, line 35, in run
    assert message == expected, f"expected message {expected!r}, got {message!r}"
  AssertionError: expected message 'Duplicate tag keys found. Please note that Tag keys are case insensitive.', got 'Duplicate tag keys found.'
The preceding assertion on the error code passed, so status 400 and Error.Code 'InvalidInput' are correct; only Error.Message diverges. 'Duplicate tag keys found. Please note that Tag keys are case insensitive.' is entirely fixed text — nothing in it comes from the request or from state.

Model grounding — smithy_query on services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json:
- SimulatePrincipalPolicy: input PolicySourceArn (required), ActionNames (required), ResourceArns, ContextEntries, ResourcePolicy, CallerArn; output SimulatePolicyResponse.EvaluationResults; errors InvalidInputException (InvalidInput, 400), NoSuchEntityException (NoSuchEntity, 404), PolicyEvaluationException (PolicyEvaluation, 500).
- EvaluationResult.MissingContextValues -> ContextKeyNamesResultListType, documented: "A list of context keys that are required by the included input policies but that were not provided by one of the input parameters. This list is used when the resource in a simulation is \"*\" ... If you include a list of resources, then any missing context values are instead included under the ResourceSpecificResults section. ... In the top-level result, this field contains the deduplicated set of missing context values across all requested resources."
- TagUser: input UserName (required, pattern ^[\w+=,.@-]+$) and Tags (required, tagListType, length 0-50); output Unit; errors InvalidInputException (awsQueryError InvalidInput, 400), NoSuchEntityException (404), LimitExceededException (409), ConcurrentModificationException (409), ServiceFailureException (500).

## Steps to Reproduce
Cause 1 (iam.sep-policy-composition-stringnotequals-missing-sourcevpce-key-deny-applies-denied-by-simulator-and-real-call):
1. CreateUser, CreateAccessKey for it, and PutUserPolicy granting iam:GetUser and iam:ListUserTags on the user's own ARN.
2. PutUserPolicy a second policy: {"Effect":"Deny","Action":"iam:ListUserTags","Resource":"<user arn>","Condition":{"StringNotEquals":{"aws:SourceVpce":"<any vpce id>"}}} — the simulated request supplies NO aws:SourceVpce.
3. SimulatePrincipalPolicy(PolicySourceArn=<user arn>, ActionNames=["iam:GetUser","iam:ListUserTags"], ResourceArns=[<user arn>]). Expect EvalDecision 'allowed' / 'explicitDeny' AND MissingContextValues == [] on the iam:ListUserTags result. Observed: the decisions are right but MissingContextValues == ['aws:SourceVpce'].
4. Repeat the call with ContextEntries=[{ContextKeyName:'aws:SourceVpce', ContextKeyValues:[<the same vpce id>], ContextKeyType:'string'}] — the decision must be 'allowed'.
5. Confirm the real signed ListUserTags as that user is AccessDenied while GetUser succeeds, then swap the Deny's operator to the positive StringEquals and confirm both the simulator and the real call report allowed/OK with MissingContextValues still empty.

Cause 2 (iam.op-contract-tag-user):
1. CreateUser, then TagUser(UserName=<user>, Tags=[{Key:'Team',Value:'payments'}]) and confirm ListUserTags reports Team=payments.
2. TagUser(UserName=<user>, Tags=[{Key:'Team',Value:'a'},{Key:'Team',Value:'b'}]).
3. The refusal must be HTTP 400, Error.Code 'InvalidInput', Error.Message exactly 'Duplicate tag keys found. Please note that Tag keys are case insensitive.' Observed message: 'Duplicate tag keys found.'
4. ListUserTags again and confirm Team is still 'payments' (the rejected call must not mutate tags).
