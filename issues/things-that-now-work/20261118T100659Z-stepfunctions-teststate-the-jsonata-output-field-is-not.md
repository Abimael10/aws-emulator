Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: stepfunctions.test-state-statename-selects-succeed-state-jsonata-output stepfunctions.test-state-malformed-jsonata-output-rejected stepfunctions.test-state-pass-next-and-end-together-rejected stepfunctions.test-state-mocked-task-never-calls-the-real-integration
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: stepfunctions.test-state-statename-selects-succeed-state-jsonata-output|run#4|AssertionError|assert top_level_resp["status"] == "SUCCEEDED", (
Issue-Key: stepfunctions.test-state-malformed-jsonata-output-rejected|_client_error#4|AssertionError|raise AssertionError(f"real AWS rejects this call, but it SUCCEEDED and returned {result!r}")
Issue-Key: stepfunctions.test-state-pass-next-and-end-together-rejected|_error#3|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded and returned {result!r}")
Issue-Key: stepfunctions.test-state-mocked-task-never-calls-the-real-integration|run#15|AssertionError|assert real["status"] == "SUCCEEDED", (
Issue-Commit: 7f8f759d74cb6fc509a9ff7e87e7c97fb9625f4c

# stepfunctions TestState: the JSONata Output field is not interpreted, malformed JSONata and Next+End are not refused, and an unmocked Task state is never invoked (4 causes)

> _Note: this issue was identified against the code base at commit `7f8f759d74cb6fc509a9ff7e87e7c97fb9625f4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TestState

## Description
FOUR net-new root causes in the TestState handler. ALL FOUR must be closed before this issue is submitted. Numbered below as `Cause N of 4`.

Adjacent open issues that do NOT cover these checks (neither carries a replay key for any of them): 20261105T115709Z-states-retry-catch-and-legacy-lambda-task-modelling-diverge.md (owns other TestState clusters) and 20261118T045852Z-states-definition-validation-is-not-enforced-on-createstatemachine.md (owns the Create/Update write path, not TestState). A similar open issue does not cover a failing check, so these are filed here and those issues are named.

Shared model basis (Smithy, com.amazonaws.sfn#TestState): required input `definition` (Definition @length 1..1048576), optional `stateName` (TestStateStateName @length 1..80) selecting one state out of a FULL state-machine definition; output carries `status` (TestExecutionStatus), `output`, `error`, `cause`, `nextState`. Modeled errors: InvalidArn, InvalidDefinition, InvalidExecutionInput, ValidationException — all client errors with httpError 400.

=== Cause 1 of 4: TestState does not interpret a JSONata state's `Output` field ===
AWS: when a state's query language is JSONata (set by the definition's top-level "QueryLanguage": "JSONata" or by the state's own QueryLanguage member), the state's `Output` member is a JSONata expression that TestState EVALUATES against the test input. For a Succeed state with Output "{% $states.input.value * 2 %}" and input {"value": 21}, TestState answers HTTP 200 with status SUCCEEDED and output 42, identically whichever of the two places QueryLanguage is set.
Emulator: TestState answers 200 with status FAILED, error 'States.Runtime' and the emulator-authored cause "the state under test carries the member 'Output', which this emulator does not interpret, so nothing was run for it" — the field is not implemented at all, and the cause text is prose AWS never emits.
Why: TestState is the API a workflow author uses to iterate on a single state. Every JSONata state that produces its result through `Output` is untestable: the caller gets FAILED/States.Runtime for a definition AWS runs successfully, so the tool reports a bug in the user's state where there is none.
Fix: the TestState handler's state-evaluation path in the states crate — resolve the effective QueryLanguage (definition-level, then state-level) and evaluate the `Output` JSONata expression, returning its result as `output` with status SUCCEEDED.
Traces: stepfunctions.test-state-statename-selects-succeed-state-jsonata-output, stepfunctions.test-state-malformed-jsonata-output-rejected.

=== Cause 2 of 4: a malformed JSONata expression is not refused with InvalidDefinition ===
AWS: TestState runs the ASL schema/JSONata parse over the submitted definition BEFORE evaluating anything, and a JSONata expression that does not parse is refused with HTTP 400, error code InvalidDefinition and the message "Invalid State Machine Definition: 'INVALID_JSONATA_EXPRESSION: <parser diagnostic> at <location>'" — for the truncated expression "{% $states.input.value * %}" on a Succeed state's Output, the diagnostic is "Unexpected end of expression" and the location is the JSON pointer /States/<StateName>/Output. Fixed text: the "Invalid State Machine Definition: '<CODE>: <message> at <location>'" template and the code INVALID_JSONATA_EXPRESSION; variable parts: the parser's diagnostic sentence and the pointer to the offending field, both derived from the submitted definition.
Emulator: the call returns HTTP 200 with status FAILED / error 'States.Runtime' (the cause from cause 1) instead of a 400 refusal, so an unparseable expression is reported as a runtime outcome rather than an invalid definition.
Why: the distinction is the whole point of the call — a 400 InvalidDefinition means 'fix your definition', a 200 FAILED means 'your definition ran and the state failed'. A client branching on ClientError vs. status takes the wrong branch, and a CI gate that asserts the refusal passes vacuously.
Fix: the TestState handler's validation step in the states crate — parse the definition's JSONata expressions before evaluation and return 400 InvalidDefinition with the INVALID_JSONATA_EXPRESSION diagnostic and the field's pointer.
Traces: stepfunctions.test-state-malformed-jsonata-output-rejected.

=== Cause 3 of 4: TestState does not run the ASL schema check — a state with both Next and End is accepted ===
AWS: TestState applies the same ASL schema check as the write path to the submitted definition. A state carrying both Next and End is refused with HTTP 400, InvalidDefinition and "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: There should only be one of the following fields: [Next, End] at /States/<StateName>'". Fixed text: that sentence and bracketed list; variable part: the state's name from the request. Nothing is run.
Emulator: TestState answers HTTP 200 with status SUCCEEDED and the input passed through as `output`, as if the definition were legal.
Why: a caller uses TestState precisely to find this class of mistake before deploying; an invalid definition that 'passes' the test tool is then shipped, and the author has no signal until the Create/Update call (which, per the companion issue, also accepts it).
Fix: the TestState handler's validation step in the states crate — run the shared ASL schema check over the full submitted definition and refuse on an ERROR diagnostic with 400 InvalidDefinition, before selecting the stateName.
Traces: stepfunctions.test-state-pass-next-and-end-together-rejected.

=== Cause 4 of 4: an unmocked Task state's integration call is never made, so TestState cannot exercise a real integration ===
AWS: TestState on a Task state with no `mock` in the request performs the state's REAL integration call under the supplied roleArn and answers 200 with status SUCCEEDED and the integration's result as `output`; supplying a mock substitutes a canned result and makes no call. Both paths are distinguishable by the side effect (the mocked run leaves no resource behind; the unmocked run does).
Emulator: the unmocked run answers status FAILED with error 'States.Runtime' and the emulator-authored cause "a 'Task' state's own work is an invocation this emulator does not make, and the request names no mock to stand in for it, so nothing was run for it" — the integration is never attempted, and the cause is prose AWS never emits.
Why: the positive control of 'mock vs. real' is unreachable: a caller cannot verify that a Task state actually talks to the integration it names, and anything exercising TestState against a real resource gets a failure for a state AWS runs to SUCCEEDED.
Fix: the TestState handler's Task-state path in the states crate — when the request carries no mock, dispatch the state's integration call (the same dispatch an execution's Task state uses) and report its result; keep the mock path short-circuiting.
Traces: stepfunctions.test-state-mocked-task-never-calls-the-real-integration.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 761/955 passed; `stepfunctions` 441/546. All four checks are listed under '### stepfunctions' and are marked neither ALREADY FILED nor OWNED.

Cause 1 — stepfunctions.test-state-statename-selects-succeed-state-jsonata-output:
  AssertionError: expected SUCCEEDED with top-level QueryLanguage, got {'error': 'States.Runtime', 'cause': "the state under test carries the member 'Output', which this emulator does not interpret, so nothing was run for it", 'status': 'FAILED', ... 'HTTPStatusCode': 200}
  expected: status SUCCEEDED, output 42 for definition {"QueryLanguage":"JSONata","StartAt":"First","States":{"First":{"Type":"Pass","Next":"Last"},"Last":{"Type":"Succeed","Output":"{% $states.input.value * 2 %}"}}}, stateName "Last", input {"value":21} — and identically with QueryLanguage on the state instead of the top level.

Cause 2 — stepfunctions.test-state-malformed-jsonata-output-rejected:
  AssertionError: real AWS rejects this call, but it SUCCEEDED and returned {'error': 'States.Runtime', 'cause': "the state under test carries the member 'Output', which this emulator does not interpret, so nothing was run for it", 'status': 'FAILED', ... 'HTTPStatusCode': 200}
  expected: HTTP 400, Error.Code 'InvalidDefinition', Error.Message "Invalid State Machine Definition: 'INVALID_JSONATA_EXPRESSION: Unexpected end of expression at /States/Last/Output'" for Output "{% $states.input.value * %}" (fixed: the template and the code; variable: the parser diagnostic and the /States/Last/Output pointer taken from the request).

Cause 3 — stepfunctions.test-state-pass-next-and-end-together-rejected:
  AssertionError: expected the call to be rejected, but it succeeded and returned {'output': '{"k": "v"}', 'status': 'SUCCEEDED', ... 'HTTPStatusCode': 200}
  expected: HTTP 400, 'InvalidDefinition', "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: There should only be one of the following fields: [Next, End] at /States/Last'" for a terminal state 'Last' carrying both End:true and Next:"First".

Cause 4 — stepfunctions.test-state-mocked-task-never-calls-the-real-integration:
  AssertionError: the unmocked run reported 'FAILED', error 'States.Runtime', cause "a 'Task' state's own work is an invocation this emulator does not make, and the request names no mock to stand in for it, so nothing was run for it"
  expected: status SUCCEEDED for a Task state with Resource "arn:aws:states:::aws-sdk:sfn:createActivity" and Parameters {"Name.$":"$.name"} when no mock is supplied, with the activity actually created.

Model references used: com.amazonaws.sfn#TestState (required `definition`; optional `stateName` TestStateStateName @length 1..80, `mock`, `roleArn`, `variables`, `context`; output `status`/`output`/`error`/`cause`/`nextState`; errors InvalidArn, InvalidDefinition, InvalidExecutionInput, ValidationException, all client/400), com.amazonaws.sfn#InvalidDefinition ('The provided Amazon States Language definition is not valid'), com.amazonaws.sfn#ValidateStateMachineDefinitionDiagnostic (location 'could be in the format /States/<StateName>/<FieldName>').

## Steps to Reproduce
Boot the emulator from this checkout and replay the behavioral suite (simparity-behavioral-aws-v1), then re-run the four checks in trace_ids. Per cause:

1. Call TestState with definition {"QueryLanguage":"JSONata","StartAt":"First","States":{"First":{"Type":"Pass","Next":"Last"},"Last":{"Type":"Succeed","Output":"{% $states.input.value * 2 %}"}}}, stateName "Last", input {"value":21}; read status and output. Repeat with the top-level QueryLanguage removed and "QueryLanguage":"JSONata" set on the Succeed state instead.
2. Call TestState with the same definition but Output "{% $states.input.value * %}"; expect a botocore ClientError with Code InvalidDefinition and the INVALID_JSONATA_EXPRESSION message, not a 200 response.
3. Call TestState with definition {"StartAt":"First","States":{"First":{"Type":"Pass","Next":"Last"},"Last":{"Type":"Pass","End":true,"Next":"First"}}}, stateName "Last", input {"k":"v"}; expect a 400 InvalidDefinition, not 200/SUCCEEDED.
4. Create a role trusted by states.amazonaws.com with permission to call sfn:CreateActivity. Call TestState with definition {"Type":"Task","Resource":"arn:aws:states:::aws-sdk:sfn:createActivity","Parameters":{"Name.$":"$.name"},"End":true}, that roleArn, an input naming an activity, and NO mock; expect status SUCCEEDED and the activity to exist afterwards. Repeat with a mock supplied and confirm no activity is created.
