Issue-Source: trace-sweep-divergence
Issue-Trace: sfn-validate-and-create-required-state-fields
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sfn-validate-and-create-required-state-fields sfn-update-state-machine-structural-definition-errors-keep-stored-definition stepfunctions-validate-transition-and-state-name-diagnostics
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: states|CreateStateMachine|4xx->2xx|$.__type
Issue-Key: states|CreateStateMachine|4xx->2xx|$.message
Issue-Key: states|CreateStateMachine|4xx->2xx|status
Issue-Key: states|UpdateStateMachine|4xx->2xx|$.__type
Issue-Key: states|UpdateStateMachine|4xx->2xx|$.message
Issue-Key: states|UpdateStateMachine|4xx->2xx|status
Issue-Key: states|DescribeStateMachine|2xx->2xx|$.revisionId
Issue-Key: states|ValidateStateMachineDefinition|2xx->2xx|$.diagnostics[*].location
Issue-Related: 20261029T171911Z-states-step-functions-response-construction-map-run-arns-carry.md
Issue-Related: 20261105T115709Z-states-retry-catch-and-legacy-lambda-task-modelling-diverge.md
Issue-Commit: 2b493b29800d7a854b42f905b4db8bab42375d7a

# States definition validation is not enforced on CreateStateMachine/UpdateStateMachine (3 causes)

> _Note: this issue was identified against the code base at commit `2b493b29800d7a854b42f905b4db8bab42375d7a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStateMachine

## Description
Three net-new root causes share one fault: the Amazon States Language schema check that AWS applies to a definition is not wired into the write path at all, and where it does run (the validator) one of its diagnostics is emulator-authored and its location is truncated. ALL THREE must be closed before this issue is submitted.

Adjacent open issues that do NOT cover these steps (they carry no replay key for them): 20261105T115709Z-states-retry-catch-and-legacy-lambda-task-modelling-diverge.md (owns other ValidateStateMachineDefinition diagnostic clusters, including the empty-ErrorEquals message and the BackoffRate minimum) and 20261103T052008Z-states-request-validation-gaps-and-non-opaque-pagination-tokens.md. A similar open issue does not cover a failure; these are filed separately and named here.

=== Cause 1 of 3: CreateStateMachine accepts a structurally invalid definition and creates the state machine ===
AWS: CreateStateMachine runs the ASL schema check on the submitted definition and refuses a definition that omits a state's required fields with HTTP 400 and the modelled error InvalidDefinition ('The provided Amazon States Language definition is not valid'), serialized with __type 'com.amazonaws.swf.service.v2.model#InvalidDefinition'. The message is the fixed template "Invalid State Machine Definition: '<CODE>: <diagnostic message> at <location>'", whose variable parts come from the first ERROR-severity diagnostic the same schema check produces: the code (e.g. SCHEMA_VALIDATION_FAILED), the diagnostic message (e.g. 'These fields are required: [<FieldName>]', naming the missing members of that state type — Resource for a Task state) and the JSON-pointer location '/States/<StateName>' taken from the submitted definition. No state machine is created. Smithy: CreateStateMachine errors include InvalidDefinition (client, httpError 400) and the required input member definition (Definition, @length 1..1048576); ValidateStateMachineDefinitionDiagnostic documents the SCHEMA_VALIDATION_FAILED code ('Schema validation reported errors') produced by the same check.
Emulator: CreateStateMachine answers 200 and creates the state machine — no __type, no message, no refusal. The definition that the emulator's own ValidateStateMachineDefinition is also failing to reject (see the owning issue above) is accepted and persisted.
Why: a client's deploy step is the last gate before a broken workflow becomes 'live'. Against AWS the bad definition never gets created and the caller sees InvalidDefinition at deploy time; against the emulator the create succeeds, so any test of a rejection path passes vacuously, and the caller goes on to start executions on a state machine AWS would never have accepted.
Fix: the CreateStateMachine handler's validation step in the states crate — run the shared ASL schema validation (the same one ValidateStateMachineDefinition uses) on the input definition and, when it yields an ERROR diagnostic, return 400 InvalidDefinition with the message template above instead of creating the resource.
Traces: sfn-validate-and-create-required-state-fields#task#8 (CreateStateMachine); the same cluster recurs at #task#9, #task#10 and #task#11 of that fixture.

=== Cause 2 of 3: UpdateStateMachine accepts a structurally invalid definition, overwrites the stored definition and mints a revision ===
AWS: UpdateStateMachine applies the same ASL schema check and refuses an invalid definition with 400 InvalidDefinition (__type 'com.amazonaws.swf.service.v2.model#InvalidDefinition') and the same fixed message template "Invalid State Machine Definition: '<CODE>: <diagnostic message> at <location>'" — for a Parallel state missing its Branches, the diagnostic message is 'These fields are required: [Branches]' and the location is '/States/<StateName>'. Because the update is refused, the update is atomic in the failure direction: the stored definition stays exactly as it was, and no new revision is produced, so a subsequent DescribeStateMachine returns the previously stored definition and no revisionId is present. Smithy: UpdateStateMachine errors include InvalidDefinition (client, 400) and ValidationException; its output's revisionId is only produced by a successful update.
Emulator: UpdateStateMachine answers 200 with no error at all, and the invalid definition becomes the stored one: the following DescribeStateMachine returns the rejected definition in `definition` instead of the previously stored one, and carries a freshly minted revisionId where AWS has none.
Why: the failure is not just a missing refusal — it is state corruption. A client that handles InvalidDefinition by keeping the old workflow in place ends up with a state machine whose stored definition is the one AWS rejected, so every later read, export or execution works from a definition that could not exist in AWS, and the revision history records an update that never happened.
Fix: the UpdateStateMachine handler's validation step and its state mutation in the states crate — validate the definition before touching stored state, return 400 InvalidDefinition with the message template above on an ERROR diagnostic, and leave the stored definition and revision untouched on refusal.
Traces: sfn-update-state-machine-structural-definition-errors-keep-stored-definition#task#4 (UpdateStateMachine, the missing refusal) and #task#5 (DescribeStateMachine, the corrupted stored definition and spurious revisionId); both recur at #task#6 and #task#7 of the same fixture.

=== Cause 3 of 3: the over-long state-name diagnostic is emulator-worded and its location is elided with an ellipsis ===
AWS: a state name may carry at most 80 characters. ValidateStateMachineDefinition reports a longer one as an ERROR-severity SCHEMA_VALIDATION_FAILED diagnostic whose message is the fixed template "Invalid State name: State '<StateName>' exceeds the 80-character limit allowed by the service" — the only variable part is the offending state name, quoted in full as it appears in the submitted definition — and whose location is the JSON pointer '/States/<StateName>', again carrying the name in full. Smithy: ValidateStateMachineDefinitionDiagnostic requires severity/code/message and carries location ('For errors specific to a field, the location could be in the format /States/<StateName>/<FieldName>'); StateName is @length 1..80. (The shape's doc does caution automation to rely on `result` rather than message wording, but the location truncation below is not a wording matter.)
Emulator: the diagnostic's location truncates the state name and appends an ellipsis character ('…'), so the pointer no longer names the state it is about; and the message is replaced by emulator prose, 'a state name may carry at most 80 characters, and this definition declares one of 81', which neither matches AWS's sentence nor quotes the name.
Why: `location` is how an authoring tool or CI annotator points the user at the offending state — a truncated, ellipsis-suffixed pointer resolves to no state in the submitted definition, so the finding cannot be mapped back to the source, and a caller that echoes the message shows text AWS never emits and that omits the state's name entirely.
Fix: the ValidateStateMachineDefinition diagnostic builder in the states crate — emit AWS's fixed message text with the full state name interpolated, and build `location` as '/States/<StateName>' without applying any display truncation.
Traces: stepfunctions-validate-transition-and-state-name-diagnostics#task#6 (ValidateStateMachineDefinition).

## Evidence
Replayed trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up = true, 80 fixtures, 893 steps, 828 passed, 65 mismatched, score 92.7%). These three clusters are net-new: no stub blocks them and no open issue carries their replay key.

Cause 1 — fixture sfn-validate-and-create-required-state-fields, step task#8 (and task#9, task#10, task#11), CreateStateMachine, expected 400 -> got 200:
  status: expected 400, got 200
  $.__type: expected 'com.amazonaws.swf.service.v2.model#InvalidDefinition', got None
  $.message: expected "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: These fields are required: [Resource] at /States/TaskState'", got None
  (fixed text: "Invalid State Machine Definition: '<CODE>: <message> at <location>'" and 'These fields are required: [<FieldName>]'; variable parts: the diagnostic code, the required member name of the state type (Resource for a Task state) and the state's JSON pointer, all derived from the submitted definition)

Cause 2 — fixture sfn-update-state-machine-structural-definition-errors-keep-stored-definition:
  step task#4 (and task#6), UpdateStateMachine, expected 400 -> got 200:
    status: expected 400, got 200
    $.__type: expected 'com.amazonaws.swf.service.v2.model#InvalidDefinition', got None
    $.message: expected "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: These fields are required: [Branches] at /States/FanOut'", got None
  step task#5 (and task#7), DescribeStateMachine, 200 -> 200:
    $.definition: expected the previously stored definition (recorded: the Pass-state definition created before the refused update), got the rejected definition the update should not have stored (recorded: the Parallel state with no Branches)
    $.revisionId: expected absent (no successful update occurred), got a freshly minted revision id

Cause 3 — fixture stepfunctions-validate-transition-and-state-name-diagnostics, step task#6, ValidateStateMachineDefinition, 200 -> 200:
  $.diagnostics[0].location: expected '/States/<the 81-character state name in full>', got the same pointer with the name cut short and an '…' ellipsis appended
  $.diagnostics[0].message: expected "Invalid State name: State '<StateName>' exceeds the 80-character limit allowed by the service", got 'a state name may carry at most 80 characters, and this definition declares one of 81'
  (fixed text: "Invalid State name: State '...' exceeds the 80-character limit allowed by the service"; variable part: the state name from the submitted definition)

Model references used: com.amazonaws.sfn#CreateStateMachine and #UpdateStateMachine (both model InvalidDefinition, client error, httpError 400; UpdateStateMachineOutput.revisionId), #InvalidDefinition ('The provided Amazon States Language definition is not valid'), #ValidateStateMachineDefinitionDiagnostic (severity/code/message required, location optional; SCHEMA_VALIDATION_FAILED = 'Schema validation reported errors'), #StateName (@length 1..80).

## Steps to Reproduce
Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1 (replay_wire_corpus). The three causes reproduce at these steps:

Cause 1: replay fixture sfn-validate-and-create-required-state-fields through step task#8 (then task#9..task#11). Call CreateStateMachine with a definition whose Task state omits the required Resource field and read the HTTP status, __type and message; then check whether the state machine was created.

Cause 2: replay fixture sfn-update-state-machine-structural-definition-errors-keep-stored-definition through steps task#4 and task#5 (then task#6/task#7). Create a valid state machine, call UpdateStateMachine with a definition whose Parallel state omits the required Branches field and read status/__type/message, then call DescribeStateMachine on the same ARN and compare `definition` against the definition stored before the update and check whether a revisionId appeared.

Cause 3: replay fixture stepfunctions-validate-transition-and-state-name-diagnostics through step task#6. Call ValidateStateMachineDefinition with a definition declaring a state whose name is 81 characters long and read diagnostics[0].location and diagnostics[0].message.
