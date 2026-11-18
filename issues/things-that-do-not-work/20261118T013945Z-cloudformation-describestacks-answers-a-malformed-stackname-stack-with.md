Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.invalid-stack-name-rejected
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.invalid-stack-name-rejected|run#16|AssertionError|assert describe_error.get("Message") == expected_describe_message, (
Issue-Commit: 9af7726e852eadd1ad5f5bb12067c11cebe7515a

# cloudformation: DescribeStacks answers a malformed StackName 'Stack with id <name> does not exist' instead of refusing it on the stackName pattern

> _Note: this issue was identified against the code base at commit `9af7726e852eadd1ad5f5bb12067c11cebe7515a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStacks

## Description
REOPENS 20261029T214321Z-cloudformation-the-stack-not-found-refusal-message-is.md: its fix left the call refused.

That issue's Cause 2 carried this very check (its Issue-Key is `cloudformation.invalid-stack-name-rejected|run#16|AssertionError|assert describe_error.get("Message") == expected_describe_message, (`). The CreateStack half of it now passes — a StackName violating the name pattern is refused with the constraint message. The DescribeStacks half was never closed: the same malformed name is still resolved as a lookup and answered 'does not exist'.

Cause 1 of 1: DescribeStacks does not apply the stackName pattern constraint before resolving the stack.

AWS: Smithy `DescribeStacksInput.StackName` targets `com.amazonaws.cloudformation#StackName`, a plain string with NO @pattern/@length trait in the model, so the constraint is enforced server-side and the refusal arrives as the service-common `ValidationError` (HTTP 400, Error.Code `ValidationError`, Error.Type `Sender`); DescribeStacks declares no not-found error shape. The message is recorded behavior only: AWS answers
`1 validation error detected: Value '<StackName>' at 'stackName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z][-a-zA-Z0-9]*|arn:[-a-zA-Z0-9:/._+]*`
Fixed text is the whole of `1 validation error detected: Value '` + `' at 'stackName' failed to satisfy constraint: Member must satisfy regular expression pattern: ` + the pattern `[a-zA-Z][-a-zA-Z0-9]*|arn:[-a-zA-Z0-9:/._+]*`. The ONLY variable part is the rejected value, echoed verbatim from the request. Note the pattern DescribeStacks reports differs from the one CreateStack reports: DescribeStacks accepts a stack ARN as well as a name, so its message names the two-alternative pattern `[a-zA-Z][-a-zA-Z0-9]*|arn:[-a-zA-Z0-9:/._+]*`, while CreateStack (name only) names `[a-zA-Z][-a-zA-Z0-9]*`. The refusal is made on the INPUT, before any stack lookup, so it fires for a name that could never identify a stack (one containing `_` or `.`).

Emulator: the same status (400) and the same Error.Code (`ValidationError`), but the request falls through to the stack lookup and the message is the emulator's not-found template `Stack with id <StackName> does not exist` — a not-found diagnosis for a request that is malformed. The CreateStack call in the same check is now refused correctly, so only the DescribeStacks input-validation path is missing.

Why: validation ORDER is part of the contract. A client is told the stack is absent when the real fault is the name, so tooling cannot distinguish 'bad name' from 'absent stack' — and because status and Error.Code are identical for both, Error.Message is the only discriminator a caller has. It also lets an unconstrained name be used to probe stack existence. CLI output comparison, error-taxonomy suites and branch logic that keys on the message all read a string AWS never emits for this input.

Fix: StackName constraint validation in the cloudformation service's DescribeStacks handler — share the same input validator CreateStack now uses, run it before the stack lookup, and emit the pattern-constraint message with DescribeStacks' name-or-ARN pattern. A well-formed but absent name must keep yielding the existing not-found refusal.

Traces: cloudformation.invalid-stack-name-rejected.

## Evidence
Check cloudformation.invalid-stack-name-rejected [mismatch], at check_cloudformation_invalid_stack_name_rejected.py line 62:

  AssertionError: expected message "1 validation error detected: Value 'sim2real-behav-cfn-bad-156ab705138b_invalid.name' at 'stackName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z][-a-zA-Z0-9]*|arn:[-a-zA-Z0-9:/._+]*", got {'Type': 'Sender', 'Code': 'ValidationError', 'Message': 'Stack with id sim2real-behav-cfn-bad-156ab705138b_invalid.name does not exist'}

Expected (AWS): HTTP 400, Error.Code `ValidationError`, Error.Message = fixed text `1 validation error detected: Value '` + <the rejected StackName from the request> + `' at 'stackName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z][-a-zA-Z0-9]*|arn:[-a-zA-Z0-9:/._+]*`.
Actual (emulator): HTTP 400, Error.Code `ValidationError`, Error.Message = `Stack with id ` + <StackName> + ` does not exist`.

The name in the quoted strings (`sim2real-behav-cfn-bad-156ab705138b_invalid.name`) is a per-run generated value filling the template's single variable slot; the requirement is the template, not that value. Its `_` and `.` are what violate the pattern.

Earlier assertions in the same check PASS, which pins the scope: the positive control (same template, a valid name) reaches CREATE_COMPLETE, and CreateStack with the malformed name is already refused ValidationError with the `...pattern: [a-zA-Z][-a-zA-Z0-9]*` message. Only the subsequent DescribeStacks read diverges.

Smithy: DescribeStacksInput.StackName -> StackName (string, no constraint traits); DescribeStacks declares no modeled not-found error, so the refusal class is the service-common ValidationError.

## Steps to Reproduce
1. Point a botocore/boto3 cloudformation client at the emulator.
2. Call CreateStack(StackName='sim2real-bad_name.1', TemplateBody='{"Resources":{"T":{"Type":"AWS::SNS::Topic"}}}'). It is correctly refused with ValidationError and the stackName pattern message (this part already works).
3. Call DescribeStacks(StackName='sim2real-bad_name.1').
4. Observe HTTP 400 ValidationError with Message 'Stack with id sim2real-bad_name.1 does not exist'. AWS instead refuses the INPUT, before any lookup, with Message "1 validation error detected: Value 'sim2real-bad_name.1' at 'stackName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z][-a-zA-Z0-9]*|arn:[-a-zA-Z0-9:/._+]*".
5. Control that must keep working: DescribeStacks with a well-formed but absent name still yields the existing not-found refusal; DescribeStacks with a stack ARN still resolves.
6. Re-replay: cloudformation.invalid-stack-name-rejected.
