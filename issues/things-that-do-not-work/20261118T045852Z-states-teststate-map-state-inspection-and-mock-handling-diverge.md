Issue-Source: trace-sweep-divergence
Issue-Trace: sfn-test-state-map-mock-error-propagation
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sfn-test-state-map-mock-error-propagation sfn-test-state-mock-exclusive-request-members sfn-test-state-mock-field-validation-modes
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: states|TestState|2xx->2xx|$.inspectionData.afterItemSelector
Issue-Key: states|TestState|2xx->2xx|$.inspectionData.afterItemsPath
Issue-Key: states|TestState|2xx->2xx|$.inspectionData.maxConcurrency
Issue-Key: states|TestState|2xx->2xx|$.inspectionData.toleratedFailureCount
Issue-Key: states|TestState|2xx->2xx|$.inspectionData.afterResultPath
Issue-Key: states|TestState|2xx->2xx|$.cause
Issue-Key: states|TestState|2xx->2xx|$.error
Issue-Key: states|TestState|2xx->2xx|$.status
Issue-Key: states|TestState|4xx->4xx|$.message
Issue-Commit: 2b493b29800d7a854b42f905b4db8bab42375d7a

# TestState Map-state inspection and mock handling diverge (4 causes)

> _Note: this issue was identified against the code base at commit `2b493b29800d7a854b42f905b4db8bab42375d7a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TestState

## Description
Four net-new root causes share one fault: the emulator's TestState handler does not model the Map-state inspection surface or the mock-input contract, so a caller debugging a single state is told something other than what the state did. ALL FOUR must be closed before this issue is submitted.

Adjacent open issues that do NOT cover these steps (they carry no replay key for them): 20261103T052008Z-states-request-validation-gaps-and-non-opaque-pagination-tokens.md (owns the TestState mock-result-schema-validation cluster, i.e. the 400 'Mock result schema validation error: Field ... must be a string' refusal) and 20261105T115709Z-states-retry-catch-and-legacy-lambda-task-modelling-diverge.md (owns the TestState errorDetails.retryIndex cluster). A similar open issue does not cover a failure; these are filed separately and named here.

=== Cause 1 of 4: TestState on a Map state omits every Map-specific inspectionData member ===
AWS: when TestState is called on a Map state with an inspectionLevel that returns inspection data, inspectionData carries the Map-specific members describing how the iteration input was derived and how the state was bounded: afterItemsPath (the effective input after the ItemsPath filter is applied), afterItemSelector (an array containing the inputs for each Map iteration, transformed by the state's ItemSelector), maxConcurrency (the Map state's max concurrency) and toleratedFailureCount (the tolerated failure threshold expressed in iterations); afterResultPath (the effective result combined with the raw state input after the ResultPath filter) is likewise populated for a Map state that produced a result. Smithy: com.amazonaws.sfn#InspectionData members afterItemsPath, afterItemSelector, afterItemBatcher, afterItemsPointer, toleratedFailureCount (InspectionToleratedFailureCount, @range min 0), toleratedFailurePercentage, maxConcurrency (InspectionMaxConcurrency, @range min 0), afterResultPath — each documented as the Map-state filter stage or bound it names. Values are taken from the submitted definition's Map state (ItemsPath/ItemSelector/MaxConcurrency/ToleratedFailureCount) and the submitted input.
Emulator: every one of those members is absent (None) on a 200 TestState response for a Map state — inspectionData carries only the non-Map stages, so afterItemsPath, afterItemSelector, maxConcurrency and toleratedFailureCount (and, on the step that produced a result, afterResultPath) are missing.
Why: TestState exists so a workflow author can see how one state transformed its input before running the workflow. For a Map state the interesting part is exactly the iteration pipeline — what ItemsPath selected, what ItemSelector produced per item, and what concurrency/failure bounds applied. A client gets an inspection object that silently claims those stages do not exist, so an authoring tool or CI check that asserts on the per-iteration inputs or the state's bounds cannot distinguish 'the state has no ItemSelector' from 'the emulator does not report it'.
Fix: the TestState handler's inspection-data construction for Map states, in the states crate — evaluate and populate the Map filter stages (afterItemsPath, afterItemSelector, afterItemBatcher/afterItemsPointer where they apply, afterResultPath) and the Map bounds (maxConcurrency, toleratedFailureCount / toleratedFailurePercentage) from the tested state's definition and input.
Traces: sfn-test-state-map-mock-error-propagation#task#1 and #task#2 (TestState).

=== Cause 2 of 4: a mocked Map iteration failure that exceeds the tolerated-failure threshold is reported as a caught task error instead of States.ExceedToleratedFailureThreshold ===
AWS: when a Map state's failed-iteration count exceeds the state's tolerated failure threshold, the Map state itself fails with the service-generated error name States.ExceedToleratedFailureThreshold and the fixed cause text 'The specified tolerated failure threshold was exceeded' — the threshold breach, not the individual iteration's error, is what terminates the state — and TestState reports status FAILED. The per-iteration error the mock raised (here a task failure with the mock's own cause text) is superseded. Smithy: TestStateOutput.status -> TestExecutionStatus enum {SUCCEEDED, FAILED, RETRIABLE, CAUGHT_ERROR}; TestStateOutput.error (SensitiveError) and cause (SensitiveCause); TestStateConfiguration.mapIterationFailureCount ('The number of Map state iterations that failed during the Map state invocation') and InspectionData.toleratedFailureCount are the two inputs this rule compares.
Emulator: status is 'CAUGHT_ERROR', error is 'States.TaskFailed' and cause is the mocked iteration's own cause text — the threshold comparison is never made, so the iteration error is propagated verbatim and treated as handled; the Map-specific inspectionData members from Cause 1 are missing on this response too.
Why: a client cannot see that the Map state breached its failure budget. A workflow author testing the threshold configuration is told the state caught a task error, so the Catch/retry design built around States.ExceedToleratedFailureThreshold is never exercised, and a test asserting on that error name fails against the emulator while passing against AWS.
Fix: the TestState handler's Map-state evaluation in the states crate — compare the mocked/observed failed-iteration count against the state's ToleratedFailureCount / ToleratedFailurePercentage and, when it is exceeded, fail the state with error States.ExceedToleratedFailureThreshold, the fixed cause text above, and status FAILED instead of surfacing the iteration's error as caught.
Traces: sfn-test-state-map-mock-error-propagation#task#3 (TestState).

=== Cause 3 of 4: the refusal for a mock naming both result and errorOutput is worded by the emulator, not by AWS ===
AWS: a TestState mock is an exclusive choice between a mocked success result and a mocked error output, and a request naming both is refused 400 with a ValidationException whose message is the fixed text 'A test mock should have only one of the following fields: [result, errorOutput].' — the message has no variable part beyond the member names it lists, which are the model's own member names. Smithy: com.amazonaws.sfn#MockInput carries result ('A JSON string containing the mocked result of the state invocation') and errorOutput ('The mocked error output when calling TestState'), and TestState models ValidationException (client error, httpError 400).
Emulator: the status (400) is right but the message is 'a mock names either a result or an errorOutput, not both' — emulator-authored prose in place of AWS's fixed sentence, with the bracketed member list dropped.
Why: the error message is the requirement, and it is what a client surfaces to the user and what SDK-level tests and golden files match on. A caller that keys on AWS's sentence (or merely shows it) gets text that does not exist in AWS, so the same bad request is diagnosed differently against the emulator than against the service.
Fix: the TestState request validation in the states crate — emit AWS's fixed message text for the mutually-exclusive mock-field refusal, naming the fields as '[result, errorOutput]'.
Traces: sfn-test-state-mock-exclusive-request-members#task#3 (TestState); the same divergence recurs at sfn-test-state-mock-field-validation-modes#task#6.

=== Cause 4 of 4: the unresolvable-JSONPath cause re-serializes the request input instead of quoting it as submitted ===
AWS: when a field's JSONPath cannot be resolved against the state input, the failure cause is the fixed template "The JSONPath '<path>' specified for the field '<fieldName>' could not be found in the input '<input>'", where <path> is the JSONPath as written in the definition, <fieldName> is the offending field name (including its '.$' suffix) and <input> is the state input AS SUBMITTED by the caller — the request's own input string, byte for byte, not a re-encoding of the parsed JSON. Smithy: TestStateOutput.cause (SensitiveCause, length 0..32768) and TestStateInput.input (SensitiveData) — recorded behavior shows the input embedded with the caller's original spacing preserved.
Emulator: the cause is otherwise identical but the embedded input is the emulator's own compact re-serialization of the parsed JSON — insignificant whitespace the caller sent (the space after the key's colon in the recorded step) is lost, so the quoted input is not the input the caller submitted.
Why: the whole point of echoing the input in this cause is to show the author the document the path was evaluated against. Once it is re-encoded, the text no longer matches what the caller sent, so a test or diff that compares the cause against the request's input string fails, and an author hunting a whitespace/formatting-sensitive payload is shown a document they never submitted.
Fix: the TestState path-resolution error construction in the states crate — carry the caller's original input string through into the cause template rather than re-serializing the parsed value.
Traces: sfn-test-state-mock-field-validation-modes#task#5 (TestState).

## Evidence
Replayed trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up = true, 80 fixtures, 893 steps, 828 passed, 65 mismatched, score 92.7%). These four clusters are net-new: no stub blocks them and no open issue carries their replay key.

Cause 1 — fixture sfn-test-state-map-mock-error-propagation, steps task#1 and task#2, TestState, 200 -> 200:
  $.inspectionData.afterItemsPath: expected the ItemsPath-filtered array (recorded: '[{"ref":"r1"}, {"ref":"r2"}, {"ref":"r3"}]'), got None
  $.inspectionData.afterItemSelector: expected the per-iteration inputs after ItemSelector (recorded: '[{"ref":"r1","pos":0}, {"ref":"r2","pos":1}, {"ref":"r3","pos":2}]'), got None
  $.inspectionData.maxConcurrency: expected the state's MaxConcurrency (recorded: 2), got None
  $.inspectionData.toleratedFailureCount: expected the state's ToleratedFailureCount (recorded: 1), got None
  (task#2 additionally) $.inspectionData.afterResultPath: expected the result combined with the raw input after ResultPath, got None

Cause 2 — fixture sfn-test-state-map-mock-error-propagation, step task#3, TestState, 200 -> 200:
  $.status: expected 'FAILED', got 'CAUGHT_ERROR'
  $.error: expected 'States.ExceedToleratedFailureThreshold', got 'States.TaskFailed'
  $.cause: expected the fixed text 'The specified tolerated failure threshold was exceeded', got the mocked iteration's cause text ('the mocked iteration raised')
  plus the Cause 1 members (afterItemSelector, afterItemsPath, maxConcurrency, toleratedFailureCount) missing on the same response

Cause 3 — fixture sfn-test-state-mock-exclusive-request-members, step task#3, TestState, 400 -> 400 (also sfn-test-state-mock-field-validation-modes#task#6):
  $.message: expected 'A test mock should have only one of the following fields: [result, errorOutput].', got 'a mock names either a result or an errorOutput, not both'
  (whole message is fixed text; the only variable-looking part, the bracketed field list, is the model's member names)

Cause 4 — fixture sfn-test-state-mock-field-validation-modes, step task#5, TestState, 200 -> 200:
  $.cause: expected "The JSONPath '$.Item.id.S' specified for the field 'ref.$' could not be found in the input '{\"Item\": \"a7\"}'", got "The JSONPath '$.Item.id.S' specified for the field 'ref.$' could not be found in the input '{\"Item\":\"a7\"}'"
  (fixed text: "The JSONPath '...' specified for the field '...' could not be found in the input '...'"; variable parts: the path and field name from the definition and the input string from the request — which is where the divergence lies, the request's own spacing being lost)

Model references used: com.amazonaws.sfn#TestState (errors InvalidDefinition/InvalidExecutionInput/ValidationException, all 400), #InspectionData (afterItemsPath, afterItemSelector, afterResultPath, maxConcurrency, toleratedFailureCount, toleratedFailurePercentage), #MockInput (result, errorOutput, fieldValidationMode), #TestStateConfiguration (mapIterationFailureCount), #TestExecutionStatus (SUCCEEDED, FAILED, RETRIABLE, CAUGHT_ERROR).

## Steps to Reproduce
Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1 (replay_wire_corpus). The four causes reproduce at these steps:

Cause 1: replay fixture sfn-test-state-map-mock-error-propagation through steps task#1 and task#2. Call TestState with a definition whose state is a Map carrying ItemsPath, ItemSelector, MaxConcurrency and ToleratedFailureCount, an input holding the items array, a mock for the iteration, and an inspectionLevel that returns inspection data; read inspectionData.afterItemsPath, .afterItemSelector, .maxConcurrency, .toleratedFailureCount (and .afterResultPath on the step that produces a result).

Cause 2: replay the same fixture through step task#3. Call TestState on the same Map state with a mocked iteration error and a failed-iteration count above the state's ToleratedFailureCount, then read status, error and cause.

Cause 3: replay fixture sfn-test-state-mock-exclusive-request-members through step task#3 (and sfn-test-state-mock-field-validation-modes through task#6). Call TestState with a mock object that sets BOTH result and errorOutput and read the 400 response's message.

Cause 4: replay fixture sfn-test-state-mock-field-validation-modes through step task#5. Call TestState with an input whose JSON contains insignificant whitespace (e.g. a space after a key's colon) and a field whose '.$' JSONPath cannot be resolved against it, then compare the returned cause's embedded input against the input string submitted in the request.
