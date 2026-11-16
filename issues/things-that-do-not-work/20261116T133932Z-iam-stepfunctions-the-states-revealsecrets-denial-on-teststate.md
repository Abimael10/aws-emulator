Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__stepfunctions.test-state-reveal-secrets-requires-permission
Issue-Key: iam__stepfunctions.test-state-reveal-secrets-requires-permission|run#10|AssertionError|assert refused == expected, f"expected {expected!r}, got {refused!r}"
Issue-Commit: 95cc8d0bd1a025cd5f4177c1d4da967d0513fc68

# iam__stepfunctions: the states:RevealSecrets denial on TestState names resource arn:aws:states:<region>:<account>:stateMachine:* instead of AWS's arn:aws:states:<region>:<account>:*:*

> _Note: this issue was identified against the code base at commit `95cc8d0bd1a025cd5f4177c1d4da967d0513fc68`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TestState

## Description
One net-new root cause, grounded in one failing behavioral check the replay report left unmarked (no ALREADY-FILED marker and no OWNED marker).

Cause 1 of 1: the AccessDeniedException that TestState(revealSecrets=true) returns to a caller lacking states:RevealSecrets names the wrong resource in its message — the emulator builds a state-machine-qualified wildcard ARN where AWS authorizes against the all-resource-types wildcard ARN.
Failing check id: iam__stepfunctions.test-state-reveal-secrets-requires-permission.

AWS: Smithy (services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json) models sfn#TestState with input {definition (required, @length 1..1048576, @sensitive), roleArn, input, inspectionLevel, revealSecrets (@default false), variables, stateName, mock, context, stateConfiguration} and modeled errors InvalidArn, InvalidDefinition, InvalidExecutionInput, ValidationException; AccessDeniedException is the service-common authorization refusal. The model says nothing about the refusal's message text, so for the message this is recorded behavior only: the recording shows HTTP 400 with error code AccessDeniedException and the message 'User: <caller principal arn> is not authorized to perform: states:RevealSecrets on resource: arn:aws:states:<region>:<account>:*:* because no identity-based policy allows the states:RevealSecrets action'. The fixed text of that template is 'User: ', ' is not authorized to perform: ', ' on resource: ', ' because no identity-based policy allows the ', ' action'; the variable parts are the caller's principal ARN, the action name (states:RevealSecrets) and the resource ARN, whose region and account come from the request — TestState is authorized against the all-resource-types wildcard (partition ':*:*' tail, i.e. any resource type, any resource), not against a stateMachine-qualified wildcard. The user name, account id and region in any single recording fill that template; they are not part of it.
Emulator: same status and same error code (AccessDeniedException), but the message's resource is a stateMachine-scoped wildcard — the replay reports actual 'User: arn:aws:iam::<account>:user/<name> is not authorized to perform: states:RevealSecrets on resource: arn:aws:states:<region>:<account>:stateMachine:* because no identity-based policy allows the states:RevealSecrets action' against expected '... on resource: arn:aws:states:<region>:<account>:*:* because ...'.
Why: a client that reads the denial to learn which resource was refused — least-privilege tooling, policy-generation helpers, log scrapers and message assertions — is told the refusal was scoped to state machines only. On AWS the reveal permission is charged against every resource type the state could touch, so a policy authored from the emulator's message grants too narrow a Resource and is still denied on AWS, and any exact message comparison fails.
Fix: the resource-ARN construction in the Step Functions authorization gate's denial message for TestState/RevealSecrets (the TestState handler's authorization step), which must emit the ':*:*' all-resource-types wildcard tail rather than ':stateMachine:*'.
Related but NOT covering this check: fixed issue 20261110T140310Z-iam-stepfunctions-teststate-revealsecrets-true-never-resolves-for.md made the gate answer at all (the refusal and its error code are now correct — this is not a reopened refusal); open issue 20261109T163411Z-iam-stepfunctions-7-net-new-causes-trust-permission.md carries the sibling states:TestState denial (its Cause 6, check iam__stepfunctions.test-state-denied-without-permission, which the report still marks OWNED) and its Cause 7 recorded the then-unenforced reveal gate under a different replay key; neither covers this message divergence, which the report left unmarked.
Traces: iam__stepfunctions.test-state-reveal-secrets-requires-permission.

## Evidence
Check iam__stepfunctions.test-state-reveal-secrets-requires-permission — verdict mismatch, at check_iam__stepfunctions_test_state_reveal_secrets_requires_permission.py line 119 'assert refused == expected'.

Expected (AWS): ('AccessDeniedException', 'User: arn:aws:iam::000000000000:user/sim2real-iamsfn-noreveal-b9cb721995f2 is not authorized to perform: states:RevealSecrets on resource: arn:aws:states:us-west-2:000000000000:*:* because no identity-based policy allows the states:RevealSecrets action')
Actual (emulator): ('AccessDeniedException', 'User: arn:aws:iam::000000000000:user/sim2real-iamsfn-noreveal-b9cb721995f2 is not authorized to perform: states:RevealSecrets on resource: arn:aws:states:us-west-2:000000000000:stateMachine:* because no identity-based policy allows the states:RevealSecrets action')

The only difference is the resource ARN's type/id tail: AWS ':*:*', emulator ':stateMachine:*'. Status, error code, principal ARN, action and surrounding fixed text all match. The user name, account id and region above are this run's values filling the template, not requirements.

The check's positive controls pass: the same caller's TestState without revealSecrets succeeds, and the caller holding states:RevealSecrets succeeds with revealSecrets=true — so the gate itself now fires; only its message's resource is wrong.

## Steps to Reproduce
1. iam:CreateUser 'noreveal-user' and iam:PutUserPolicy with {"Effect":"Allow","Action":"states:TestState","Resource":"*"}; iam:CreateAccessKey for it.
2. Signed with that key, call sfn:TestState(definition='{"Type": "Pass", "Result": {"ok": true}, "End": true}', input="{}", revealSecrets=true).
3. Observe the refusal: error code AccessDeniedException (correct) with message '... on resource: arn:aws:states:<region>:<account>:stateMachine:* because no identity-based policy allows the states:RevealSecrets action'.
4. AWS answers the same code with '... on resource: arn:aws:states:<region>:<account>:*:* because no identity-based policy allows the states:RevealSecrets action'.
5. Control: the identical call with revealSecrets omitted succeeds for the same caller (status SUCCEEDED), and a second user whose policy also allows states:RevealSecrets succeeds with revealSecrets=true.
