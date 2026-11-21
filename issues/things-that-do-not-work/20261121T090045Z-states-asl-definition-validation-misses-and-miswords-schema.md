Issue-Source: trace-sweep-divergence
Issue-Trace: stepfunctions-validate-choice-state-rule-diagnostics
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: stepfunctions-validate-choice-state-rule-diagnostics stepfunctions-validate-task-map-parallel-bound-diagnostics stepfunctions-validate-top-level-field-diagnostics stepfunctions-validate-transition-and-state-name-diagnostics stepfunctions-validate-wait-state-field-diagnostics sfn-validate-and-create-required-state-fields
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: states|ValidateStateMachineDefinition|2xx->2xx|$.diagnostics[*]
Issue-Key: states|ValidateStateMachineDefinition|2xx->2xx|$.result
Issue-Key: states|ValidateStateMachineDefinition|2xx->2xx|$.diagnostics[*].message
Issue-Key: states|CreateStateMachine|4xx->4xx|$.message
Issue-Commit: 229c05c4a85b849588e172b17631c1308ea16fa2

# states: ASL definition validation misses and miswords schema diagnostics

> _Note: this issue was identified against the code base at commit `229c05c4a85b849588e172b17631c1308ea16fa2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ValidateStateMachineDefinition

## Description
The emulator's Amazon States Language schema validator is incomplete and, where it does fire, authors its own English instead of the sentences AWS returns. The same validator backs ValidateStateMachineDefinition and the definition check inside CreateStateMachine, so both surfaces diverge. SEVEN causes, all on service `states`.

Smithy (services/service-com-amazonaws-sfn-awsstepfunctions): ValidateStateMachineDefinition output members `result` (required, enum ValidateStateMachineDefinitionResultCode = OK|FAIL) and `diagnostics` (required, list of ValidateStateMachineDefinitionDiagnostic{severity required, code required, message required, location}); the diagnostic shape documents the error codes SCHEMA_VALIDATION_FAILED, MISSING_END_STATE, MISSING_TRANSITION_TARGET and documents `location` as "/States/<StateName>/<FieldName>". The shape's note warns clients not to rely on diagnostic order/count/wording, but the recorded AWS responses are the reference the corpus asserts against, so the wording causes below are reported as fidelity defects at MEDIUM while the missing-verdict causes are HIGH.

Cause 1 of 7: a schema-invalid definition is accepted as valid (result OK, no diagnostics).
AWS: for a definition that violates the ASL schema, ValidateStateMachineDefinition answers 200 with `result` = "FAIL" and at least one ERROR diagnostic. In the representative recording a Choice state with an empty `Choices` array yields {severity: ERROR, code: SCHEMA_VALIDATION_FAILED, location: "/States/<choice-state-name>/Choices", message: "Value cannot be empty"} — the message text is fixed, the location is built from the offending state's name and field. The same FAIL-plus-diagnostic shape is recorded for the Task/Map/Parallel field-bound rules, the top-level field rules, the transition rules and the Wait-state field rules.
Emulator: 200 with `result` = "OK" and `diagnostics` empty (expected diagnostics[0] missing) across 16 recorded steps covering five rule families.
Why: a client that validates a definition before deploying it is told the workflow is well-formed, then either CreateStateMachine refuses it later or the workflow misbehaves at runtime; CI gates keyed on `result` pass on definitions AWS rejects.
Fix: the ASL schema-validation pass behind the ValidateStateMachineDefinition handler (the shared definition validator), which must emit a diagnostic per violated rule and set result=FAIL whenever any ERROR diagnostic exists.
Traces: stepfunctions-validate-choice-state-rule-diagnostics#task#2 (representative; cluster also covers task#4, task#5, stepfunctions-validate-task-map-parallel-bound-diagnostics#task#2/#4/#5/#7/#8/#9/#10, stepfunctions-validate-top-level-field-diagnostics#task#2/#3, stepfunctions-validate-transition-and-state-name-diagnostics#task#7, stepfunctions-validate-wait-state-field-diagnostics#task#3/#4/#6).

Cause 2 of 7: a Choice rule with no Next produces neither of AWS's two diagnostics.
AWS: a Choices entry lacking `Next` yields TWO ERROR diagnostics and result=FAIL — {code: SCHEMA_VALIDATION_FAILED, location: "/States/<state>/Choices[<i>]", message: "These fields are required: [Next]"} and {code: MISSING_TRANSITION_TARGET, location: "/States/<state>/Choices[<i>]/Next", message: "Missing transition pointer"}. Both messages are fixed text; the bracketed index and the state name come from the submitted definition.
Emulator: result "OK", diagnostics empty.
Why: the single most common Choice authoring mistake is reported as valid, and the MISSING_TRANSITION_TARGET code never appears, so tooling that keys on it is dead code.
Fix: the Choice-rule branch of the same schema validator plus the transition-target resolution pass.
Traces: stepfunctions-validate-choice-state-rule-diagnostics#task#3.

Cause 3 of 7: MISSING_END_STATE is never emitted.
AWS: a workflow in which no reachable state terminates (no state with "End": true and no Succeed/Fail state) yields {severity: ERROR, code: MISSING_END_STATE, message: "Workflow has no terminal state"} with NO `location` member — fixed text, no variable part. In the recording it is the third diagnostic alongside the top-level field diagnostics.
Emulator: that diagnostic is absent (diagnostics[2] missing); the other top-level diagnostics are produced.
Why: a workflow that can never complete is reported as having only cosmetic problems.
Fix: a terminal-state reachability check in the schema validator.
Traces: stepfunctions-validate-top-level-field-diagnostics#task#4.

Cause 4 of 7: a Map state with no iteration body is diagnosed with the wrong rule.
AWS: a Map state carrying neither `ItemProcessor` nor `Iterator` is reported as a mutually-exclusive-field violation: message "This object should have only one of the following fields: [ItemProcessor, Iterator]", code SCHEMA_VALIDATION_FAILED, location "/States/<map-state-name>". The sentence and the bracketed field list are fixed text; only the location's state name varies.
Emulator: same code/location but message "These fields are required: [ItemProcessor]" — the required-fields rule, naming only one of the two accepted members.
Why: the diagnostic tells the author that `Iterator` is not accepted at all, when it is the (deprecated) alternative; a client matching on AWS's sentence does not match.
Fix: the Map-state rule table in the schema validator — ItemProcessor/Iterator is a one-of group, not a required single field.
Traces: sfn-validate-and-create-required-state-fields#task#7.

Cause 5 of 7: CreateStateMachine's InvalidDefinition message carries the same mis-worded clause.
AWS: CreateStateMachine refuses the same definition with 400 and message "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: This object should have only one of the following fields: [ItemProcessor, Iterator] at /States/<map-state-name>'" — the "Invalid State Machine Definition: '<CODE>: <diagnostic message> at <location>'" envelope is fixed text, the code, diagnostic sentence and location come from the diagnostic produced for the definition.
Emulator: 400 with the same envelope but the diagnostic clause "SCHEMA_VALIDATION_FAILED: These fields are required: [ItemProcessor] at /States/<map-state-name>".
Why: the refusal reason a caller logs or matches on differs from AWS's, and it misdirects the fix.
Fix: shared with Cause 4 — the CreateStateMachine refusal is rendered from the validator's diagnostic, so correcting the rule corrects both; the envelope rendering itself is correct.
Traces: sfn-validate-and-create-required-state-fields#task#11.

Cause 6 of 7: a Pass state with neither Next nor End gets emulator-authored prose.
AWS: message "This object should have only one of the following fields: [Next, End]", code SCHEMA_VALIDATION_FAILED, severity ERROR, location "/States/<state-name>" — fixed sentence plus fixed bracketed field list; only the location varies.
Emulator: same code/severity/location but message "state '<name>' is a '<Type>' state, so it must carry either 'Next' or \"End\": true" — hand-written prose that interpolates the state name and type.
Why: the diagnostic message is part of the API contract a client reads or matches; emulator-specific wording cannot be matched and leaks the emulator's own vocabulary.
Fix: the transition-rule diagnostics in the schema validator must render AWS's one-of sentence rather than per-state-type prose.
Traces: stepfunctions-validate-transition-and-state-name-diagnostics#task#2.

Cause 7 of 7: a Succeed state carrying Next produces an EXTRA diagnostic.
AWS: this definition yields exactly one diagnostic in the recorded response; there is no second element.
Emulator: appends diagnostics[1] = {severity: ERROR, code: SCHEMA_VALIDATION_FAILED, location: "/States/<state-name>/Next", message: "state '<name>' is a 'Succeed' state, which ends an execution and must not carry 'Next'"} — again emulator-authored prose, and a diagnostic AWS does not report for this definition.
Why: a caller that enumerates diagnostics sees a problem count and a location AWS never reports, so per-diagnostic remediation tooling reports a phantom error.
Fix: the terminal-state rule in the schema validator should not raise a second diagnostic once the condition is already covered, and must not author its own wording.
Traces: stepfunctions-validate-transition-and-state-name-diagnostics#task#3.

Related open issues (neither owns these replay keys): 20261029T171911Z-states-step-functions-response-construction-map-run-arns-carry.md covers Map Run response construction only.

## Evidence
Replay of trace set simparity-wire-aws-v1 (80 fixtures, 893 steps, 29 mismatched) — all expected values below are from the recorded AWS responses, all actual values from the emulator.

Cause 1 — ValidateStateMachineDefinition, expected 200 / actual 200:
  $.result: expected 'FAIL', got 'OK'
  $.diagnostics[0]: expected {'code': 'SCHEMA_VALIDATION_FAILED', 'location': '/States/C/Choices', 'message': 'Value cannot be empty', 'severity': 'ERROR'}, got None
  cluster affected_count = 16 steps across stepfunctions-validate-choice-state-rule-diagnostics, -task-map-parallel-bound-diagnostics, -top-level-field-diagnostics, -transition-and-state-name-diagnostics, -wait-state-field-diagnostics.

Cause 2 — ValidateStateMachineDefinition, 200/200:
  $.result: expected 'FAIL', got 'OK'
  $.diagnostics[0]: expected {'code': 'SCHEMA_VALIDATION_FAILED', 'location': '/States/C/Choices[0]', 'message': 'These fields are required: [Next]', 'severity': 'ERROR'}, got None
  $.diagnostics[1]: expected {'code': 'MISSING_TRANSITION_TARGET', 'location': '/States/C/Choices[0]/Next', 'message': 'Missing transition pointer', 'severity': 'ERROR'}, got None

Cause 3 — ValidateStateMachineDefinition, 200/200:
  $.diagnostics[2]: expected {'code': 'MISSING_END_STATE', 'message': 'Workflow has no terminal state', 'severity': 'ERROR'}, got None

Cause 4 — ValidateStateMachineDefinition, 200/200:
  $.diagnostics[0].message: expected 'This object should have only one of the following fields: [ItemProcessor, Iterator]', got 'These fields are required: [ItemProcessor]'

Cause 5 — CreateStateMachine, expected 400 / actual 400:
  $.message: expected "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: This object should have only one of the following fields: [ItemProcessor, Iterator] at /States/MapState'", got "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: These fields are required: [ItemProcessor] at /States/MapState'"

Cause 6 — ValidateStateMachineDefinition, 200/200:
  $.diagnostics[0]: expected {'code': 'SCHEMA_VALIDATION_FAILED', 'location': '/States/A', 'message': 'This object should have only one of the following fields: [Next, End]', 'severity': 'ERROR'}, got {'code': 'SCHEMA_VALIDATION_FAILED', 'location': '/States/A', 'message': 'state \'A\' is a \'Pass\' state, so it must carry either \'Next\' or "End": true', 'severity': 'ERROR'}

Cause 7 — ValidateStateMachineDefinition, 200/200:
  $.diagnostics[1]: expected None, got {'code': 'SCHEMA_VALIDATION_FAILED', 'location': '/States/A/Next', 'message': "state 'A' is a 'Succeed' state, which ends an execution and must not carry 'Next'", 'severity': 'ERROR'}

Model evidence: ValidateStateMachineDefinitionResultCode enum = [OK, FAIL]; output `result` and `diagnostics` both required; ValidateStateMachineDefinitionDiagnostic documents codes SCHEMA_VALIDATION_FAILED / MISSING_END_STATE / MISSING_TRANSITION_TARGET and the "/States/<StateName>/<FieldName>" location form.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 1: fixture stepfunctions-validate-choice-state-rule-diagnostics, step task#2 — ValidateStateMachineDefinition with a Choice state whose Choices array is empty. Expect result=FAIL plus a SCHEMA_VALIDATION_FAILED/'Value cannot be empty' diagnostic at /States/<state>/Choices; observe result=OK and no diagnostics. Same for the other 15 steps in that cluster (task#4, task#5 and the task-map-parallel-bound / top-level-field / transition-and-state-name / wait-state-field fixtures).
3. Cause 2: stepfunctions-validate-choice-state-rule-diagnostics#task#3 — a Choices[0] entry with no Next.
4. Cause 3: stepfunctions-validate-top-level-field-diagnostics#task#4 — a definition with no terminal state; expect the MISSING_END_STATE diagnostic.
5. Cause 4: sfn-validate-and-create-required-state-fields#task#7 — validate a Map state carrying neither ItemProcessor nor Iterator; compare diagnostics[0].message.
6. Cause 5: sfn-validate-and-create-required-state-fields#task#11 — CreateStateMachine with that same definition; compare the 400 message's diagnostic clause.
7. Cause 6: stepfunctions-validate-transition-and-state-name-diagnostics#task#2 — a Pass state with neither Next nor End; compare diagnostics[0].message.
8. Cause 7: stepfunctions-validate-transition-and-state-name-diagnostics#task#3 — a Succeed state carrying Next; assert no diagnostics[1] is emitted.
9. All seven must pass before closing: re-replay every fixture#step listed above, not only the representative.
