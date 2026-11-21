Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: stepfunctions.create-state-machine-choice-rule-without-next-rejected stepfunctions.create-state-machine-definition-version-other-than-1-0-rejected stepfunctions.create-state-machine-map-tolerated-failure-percentage-over-100-rejected stepfunctions.create-state-machine-next-and-end-together-rejected stepfunctions.create-state-machine-task-timeout-seconds-zero-rejected stepfunctions.create-state-machine-parallel-branch-next-to-outer-state-rejected stepfunctions.create-state-machine-state-name-over-80-characters-rejected stepfunctions.update-state-machine-succeed-with-next-rejected-keeps-definition stepfunctions.update-state-machine-unknown-state-type-rejected-keeps-definition stepfunctions.update-state-machine-wait-seconds-and-timestamp-together-rejected
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: stepfunctions.create-state-machine-choice-rule-without-next-rejected|_refused_create#8|AssertionError|raise AssertionError(f"expected CreateStateMachine to be refused, it created {name!r}")
Issue-Key: stepfunctions.create-state-machine-definition-version-other-than-1-0-rejected|_refused_create#7|AssertionError|raise AssertionError(f"expected CreateStateMachine to be refused, it created {name!r}")
Issue-Key: stepfunctions.create-state-machine-map-tolerated-failure-percentage-over-100-rejected|_refused_create#6|AssertionError|raise AssertionError(f"expected CreateStateMachine to be refused, it created {name!r}")
Issue-Key: stepfunctions.create-state-machine-next-and-end-together-rejected|_refused_create#10|AssertionError|raise AssertionError(f"expected CreateStateMachine to be refused, it created {name!r}")
Issue-Key: stepfunctions.create-state-machine-task-timeout-seconds-zero-rejected|_refused_create#14|AssertionError|raise AssertionError(f"expected CreateStateMachine to be refused, it created {name!r}")
Issue-Key: stepfunctions.create-state-machine-parallel-branch-next-to-outer-state-rejected|run#12|AssertionError|assert message == _EXPECTED, f"expected {_EXPECTED!r}, got {message!r}"
Issue-Key: stepfunctions.create-state-machine-state-name-over-80-characters-rejected|run#13|AssertionError|assert message == _EXPECTED, f"expected {_EXPECTED!r}, got {message!r}"
Issue-Key: stepfunctions.update-state-machine-succeed-with-next-rejected-keeps-definition|run#9|AssertionError|assert message == _EXPECTED, f"expected {_EXPECTED!r}, got {message!r}"
Issue-Key: stepfunctions.update-state-machine-unknown-state-type-rejected-keeps-definition|run#8|AssertionError|assert message == _EXPECTED, f"expected {_EXPECTED!r}, got {message!r}"
Issue-Key: stepfunctions.update-state-machine-wait-seconds-and-timestamp-together-rejected|_error#6|AssertionError|raise AssertionError(f"expected the call to be refused, it succeeded with {result!r}")
Issue-Commit: 7f8f759d74cb6fc509a9ff7e87e7c97fb9625f4c

# stepfunctions: ASL definition validation on CreateStateMachine/UpdateStateMachine misses five rules and mis-words five InvalidDefinition diagnostics (10 causes)

> _Note: this issue was identified against the code base at commit `7f8f759d74cb6fc509a9ff7e87e7c97fb9625f4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStateMachine

## Description
TEN net-new root causes in the Amazon States Language schema check that CreateStateMachine and UpdateStateMachine must run over the submitted `definition`. Six are missing rules (the definition is ACCEPTED and the resource created/updated where AWS answers 400 InvalidDefinition); four are refusals that happen but whose InvalidDefinition message is not AWS's. ALL TEN must be closed before this issue is submitted. Numbered below as `Cause N of 10`.

Adjacent open issue that does NOT cover these checks (it carries no replay key for any of them, and names only the Task-missing-Resource, Parallel-missing-Branches and ValidateStateMachineDefinition state-name clusters): 20261118T045852Z-states-definition-validation-is-not-enforced-on-createstatemachine.md. Also adjacent and non-covering: 20261105T115709Z-states-retry-catch-and-legacy-lambda-task-modelling-diverge.md. A similar open issue does not cover a failing check, so these are filed here and those issues are named rather than relied on.

Shared model basis (Smithy, com.amazonaws.sfn#AWSStepFunctions): CreateStateMachine and UpdateStateMachine both model InvalidDefinition as a client error with httpError 400, and both take `definition` (Definition, @length 1..1048576). InvalidDefinition is documented as 'The provided Amazon States Language definition is not valid'. Its message is the fixed template "Invalid State Machine Definition: '<CODE>: <diagnostic message> at <location>'", repeated comma-separated when the schema check yields more than one ERROR diagnostic; <CODE> is a diagnostic code such as SCHEMA_VALIDATION_FAILED, MISSING_TRANSITION_TARGET, MISSING_END_STATE or INVALID_STATE_NAME, and <location> is a JSON pointer into the SUBMITTED definition. Every state name, member name, index and limit quoted below is a variable part taken from the request; only the sentence skeletons are fixed text.

=== Cause 1 of 10: a Choice rule with no Next is accepted ===
AWS: a Choice rule must carry a transition. CreateStateMachine refuses a Choices[i] entry with no Next with 400 InvalidDefinition and the two-diagnostic message "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: These fields are required: [Next] at /States/<ChoiceStateName>/Choices[<i>], MISSING_TRANSITION_TARGET: Missing transition pointer at /States/<ChoiceStateName>/Choices[<i>]/Next'". Fixed text: "These fields are required: [Next]", "Missing transition pointer". Variable parts: the Choice state's name and the rule index, both read from the request. Nothing is created.
Emulator: CreateStateMachine answers 200 and creates the state machine; no error, and the machine is subsequently listable.
Why: the deploy call is a client's last gate before a broken workflow goes live. A caller that handles InvalidDefinition sees success, starts executions against a definition AWS would never accept, and any CI assertion on the rejection path passes vacuously.
Fix: the CreateStateMachine handler's definition-validation step in the states crate — add the Choice-rule transition rule to the shared ASL schema check and refuse before any state is written.
Traces: stepfunctions.create-state-machine-choice-rule-without-next-rejected.

=== Cause 2 of 10: a definition whose top-level Version is not 1.0 is accepted ===
AWS: the only legal top-level Version is 1.0. CreateStateMachine refuses any other value with 400 InvalidDefinition and "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Value should be one of the following: [1.0] at /Version'" — entirely fixed text, with the location the fixed pointer /Version. Nothing is created.
Emulator: answers 200 and creates the state machine.
Why: a caller can ship a definition written against a language version the service does not implement and get no signal at all; the executions that follow run under 1.0 semantics the author did not ask for.
Fix: the CreateStateMachine handler's definition-validation step — enforce the top-level Version enumeration in the shared ASL schema check.
Traces: stepfunctions.create-state-machine-definition-version-other-than-1-0-rejected.

=== Cause 3 of 10: a Map ToleratedFailurePercentage above 100 is accepted ===
AWS: ToleratedFailurePercentage is bounded at 100.0. CreateStateMachine refuses a larger value with 400 InvalidDefinition and "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Maximum value is 100.0 at /States/<MapStateName>/ToleratedFailurePercentage'". Fixed text: "Maximum value is 100.0". Variable part: the Map state's name from the request. Nothing is created.
Emulator: answers 200 and creates the state machine.
Why: the out-of-range tolerance is silently kept, so a distributed Map's failure budget is whatever the emulator makes of a percentage above 100 — a caller cannot tell a mis-authored budget from an accepted one.
Fix: the CreateStateMachine handler's definition-validation step — enforce the numeric range of Map ToleratedFailurePercentage in the shared ASL schema check.
Traces: stepfunctions.create-state-machine-map-tolerated-failure-percentage-over-100-rejected.

=== Cause 4 of 10: a state carrying both Next and End is accepted by CreateStateMachine ===
AWS: Next and End are mutually exclusive on a state. CreateStateMachine refuses with 400 InvalidDefinition and "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: There should only be one of the following fields: [Next, End] at /States/<StateName>'". Fixed text: "There should only be one of the following fields: [Next, End]". Variable part: the offending state's name. Nothing is created.
Emulator: answers 200 and creates the state machine.
Why: the stored definition is ambiguous about where the state goes, so the execution's control flow is emulator-defined and differs from the author's intent with no error anywhere.
Fix: the CreateStateMachine handler's definition-validation step — enforce the Next/End exclusivity rule in the shared ASL schema check.
Traces: stepfunctions.create-state-machine-next-and-end-together-rejected.

=== Cause 5 of 10: a Task TimeoutSeconds of 0 is accepted ===
AWS: TimeoutSeconds has a floor of 1. CreateStateMachine refuses 0 with 400 InvalidDefinition and "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Minimum value is 1 at /States/<TaskStateName>/TimeoutSeconds'". Fixed text: "Minimum value is 1". Variable part: the Task state's name. Nothing is created.
Emulator: answers 200 and creates the state machine.
Why: a zero timeout is nonsense AWS never stores; the emulator keeps it, so the task's timeout behaviour (immediate States.Timeout, no timeout at all, or something else) is unspecified and untestable against the real service.
Fix: the CreateStateMachine handler's definition-validation step — enforce the TimeoutSeconds minimum in the shared ASL schema check.
Traces: stepfunctions.create-state-machine-task-timeout-seconds-zero-rejected.

=== Cause 6 of 10: the MISSING_END_STATE diagnostic for a Parallel branch reports a branch pointer instead of null ===
AWS: a Parallel branch is its own scope, so a branch state whose Next names an outer state both dangles and leaves the branch with no terminal state. CreateStateMachine refuses with 400 InvalidDefinition and the two-diagnostic message "Invalid State Machine Definition: 'MISSING_TRANSITION_TARGET: Missing 'Next' target: <TargetName> at /States/<ParallelName>/Branches[<i>]/States/<StateName>/Next, MISSING_END_STATE: Workflow has no terminal state at null'": the MISSING_END_STATE diagnostic's location is the literal text `null`, not a pointer. Fixed text: "Missing 'Next' target:", "Workflow has no terminal state", and the `null` location. Variable parts: the dangling target name and the branch/state pointer.
Emulator: the refusal happens and the first diagnostic matches, but the second reads "MISSING_END_STATE: Workflow has no terminal state at /States/<ParallelName>/Branches[<i>]" — a branch pointer where AWS emits `null`.
Why: a client that parses the diagnostic list (an authoring tool, a CI annotator) keys on the location to decide what to highlight; a pointer where AWS emits `null` makes the emulator's message a different string for the same defect, so exact-match assertions and message-diffing tooling both break.
Fix: the InvalidDefinition message/diagnostic builder in the states crate — emit the literal `null` location for a MISSING_END_STATE diagnostic that is not bound to a single field.
Traces: stepfunctions.create-state-machine-parallel-branch-next-to-outer-state-rejected.

=== Cause 7 of 10: the INVALID_STATE_NAME diagnostic is emulator-worded and its location is truncated with an ellipsis ===
AWS: a state name may carry at most 80 characters (Smithy: StateName @length 1..80). CreateStateMachine refuses a longer one with 400 InvalidDefinition and "Invalid State Machine Definition: 'INVALID_STATE_NAME: Invalid State name: State '<StateName>' exceeds the 80-character limit allowed by the service at /States/<StateName>'". Fixed text: "Invalid State name: State '...' exceeds the 80-character limit allowed by the service". Variable part: the offending state name, quoted IN FULL in both the sentence and the location pointer. Nothing is created.
Emulator: the refusal happens but the message is emulator prose — "INVALID_STATE_NAME: a state name may carry at most 80 characters, and this definition declares one of <n> at /States/<name cut short>…" — which never quotes the name in the sentence and appends the ellipsis character '…' to a truncated location pointer, so the pointer resolves to no state in the submitted definition.
Why: the location is how a tool maps the finding back to the source; a truncated, ellipsis-suffixed pointer names nothing, and a caller that surfaces the message shows text AWS never emits and that omits the offending name entirely. (The ValidateStateMachineDefinition facet of this same wording is already owned by 20261118T045852Z-states-definition-validation-is-not-enforced-on-createstatemachine.md cause 3; the CreateStateMachine InvalidDefinition message is this cause and is not covered there.)
Fix: the InvalidDefinition message builder in the states crate — use AWS's fixed sentence with the full state name interpolated, and build the location as /States/<StateName> with no display truncation.
Traces: stepfunctions.create-state-machine-state-name-over-80-characters-rejected.

=== Cause 8 of 10: UpdateStateMachine appends an extra emulator-authored diagnostic to the Succeed-with-Next refusal ===
AWS: a Succeed state does not support Next. UpdateStateMachine refuses with 400 InvalidDefinition and EXACTLY ONE diagnostic: "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Field 'Next' is not supported at /States/<StateName>'". Fixed text: "Field 'Next' is not supported". Variable part: the state's name. The stored definition and revisionId are unchanged.
Emulator: refuses, but the message carries a second, emulator-authored diagnostic appended after a comma: ", SCHEMA_VALIDATION_FAILED: state '<StateName>' is a '<Type>' state, which ends an execution and must not carry 'Next' at /States/<StateName>/Next" — prose AWS never emits, reporting one defect twice.
Why: the message is a requirement, and a caller asserting on it (or diffing refusals between environments) sees a string AWS never produces; a tool that counts diagnostics reports two problems where there is one.
Fix: the UpdateStateMachine definition-validation diagnostic builder in the states crate — emit only the unsupported-field diagnostic for this rule and drop the duplicate state-type sentence.
Traces: stepfunctions.update-state-machine-succeed-with-next-rejected-keeps-definition.

=== Cause 9 of 10: the unknown-state-Type refusal message is emulator-worded and lists the types in the wrong order ===
AWS: a state's Type must be one of the eight ASL types. UpdateStateMachine refuses an unknown Type with 400 InvalidDefinition and "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: The field 'Type' should have one of these values: [Task, Wait, Pass, Succeed, Fail, Choice, Parallel, Map] at /States/<StateName>/Type'". Fixed text: the whole sentence INCLUDING the bracketed value list in that exact order (Task, Wait, Pass, Succeed, Fail, Choice, Parallel, Map); it does not echo the rejected value. Variable part: the state's name in the location only.
Emulator: refuses, but with "SCHEMA_VALIDATION_FAILED: state '<StateName>' is of type '<RejectedType>', which is not one of the state types the Amazon States Language defines (Choice, Fail, Map, Parallel, Pass, Succeed, Task, Wait) at /States/<StateName>/Type" — emulator prose, the rejected value echoed, parentheses instead of brackets, and the eight types sorted alphabetically rather than in AWS's order.
Why: same as cause 8 — the refusal message is part of the contract; an exact-match assertion or a message-based client branch written against AWS fails here, and the value list a caller might parse out is in a different order and syntax.
Fix: the UpdateStateMachine definition-validation diagnostic builder in the states crate — emit AWS's fixed sentence with the value list verbatim and in AWS's order.
Traces: stepfunctions.update-state-machine-unknown-state-type-rejected-keeps-definition.

=== Cause 10 of 10: UpdateStateMachine accepts a Wait state naming both Seconds and Timestamp, and stores it ===
AWS: a Wait state must name exactly one of SecondsPath, Seconds, TimestampPath, Timestamp. UpdateStateMachine refuses two of them with 400 InvalidDefinition and "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: There should only be one of the following fields: [SecondsPath, Seconds, TimestampPath, Timestamp] at /States/<WaitStateName>'". Fixed text: the sentence and the bracketed list in that order. Variable part: the Wait state's name. Because the update is refused, the stored definition and the revision are untouched.
Emulator: answers 200 with an updateDate and a freshly minted revisionId, and the rejected definition becomes the stored one — a subsequent DescribeStateMachine returns the invalid definition.
Why: this is not only a missing refusal but state corruption: a client whose rollback strategy is 'InvalidDefinition means the old workflow stands' ends up with a stored definition AWS could not hold and a revision recording an update that never happened.
Fix: the UpdateStateMachine handler's validation step and its state mutation in the states crate — run the Wait-field exclusivity rule before touching stored state, and leave the definition and revision untouched on refusal.
Traces: stepfunctions.update-state-machine-wait-seconds-and-timestamp-together-rejected.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 761/955 passed; service `stepfunctions` 441/546. All ten checks below are listed under '### stepfunctions' and are marked neither ALREADY FILED nor OWNED.

Cause 1 — stepfunctions.create-state-machine-choice-rule-without-next-rejected:
  AssertionError: expected CreateStateMachine to be refused, it created 'sim2real-behav-sm-bad-<id>'
  expected code InvalidDefinition (HTTP 400) and message "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: These fields are required: [Next] at /States/C/Choices[0], MISSING_TRANSITION_TARGET: Missing transition pointer at /States/C/Choices[0]/Next'" (C and the index 0 are the request's values); got HTTP 200 and a created state machine.

Cause 2 — stepfunctions.create-state-machine-definition-version-other-than-1-0-rejected:
  AssertionError: expected CreateStateMachine to be refused, it created 'sim2real-behav-sm-bad-<id>'
  expected InvalidDefinition / "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Value should be one of the following: [1.0] at /Version'" for a definition declaring Version '2.0'; got 200.

Cause 3 — stepfunctions.create-state-machine-map-tolerated-failure-percentage-over-100-rejected:
  AssertionError: expected CreateStateMachine to be refused, it created 'sim2real-behav-sm-bad-<id>'
  expected InvalidDefinition / "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Maximum value is 100.0 at /States/M/ToleratedFailurePercentage'" for ToleratedFailurePercentage 101 on a DISTRIBUTED Map named M; got 200.

Cause 4 — stepfunctions.create-state-machine-next-and-end-together-rejected:
  AssertionError: expected CreateStateMachine to be refused, it created 'sim2real-behav-sm-bad-<id>'
  expected InvalidDefinition / "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: There should only be one of the following fields: [Next, End] at /States/A'"; got 200.

Cause 5 — stepfunctions.create-state-machine-task-timeout-seconds-zero-rejected:
  AssertionError: expected CreateStateMachine to be refused, it created 'sim2real-behav-sm-bad-<id>'
  expected InvalidDefinition / "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Minimum value is 1 at /States/T/TimeoutSeconds'" for a Task state T with TimeoutSeconds 0; got 200.

Cause 6 — stepfunctions.create-state-machine-parallel-branch-next-to-outer-state-rejected:
  expected "Invalid State Machine Definition: 'MISSING_TRANSITION_TARGET: Missing 'Next' target: D at /States/P/Branches[0]/States/X/Next, MISSING_END_STATE: Workflow has no terminal state at null'"
  got      "Invalid State Machine Definition: 'MISSING_TRANSITION_TARGET: Missing 'Next' target: D at /States/P/Branches[0]/States/X/Next, MISSING_END_STATE: Workflow has no terminal state at /States/P/Branches[0]'"
  (fixed text: "Workflow has no terminal state" plus the literal location `null`; D, P, X and the branch index are the request's values)

Cause 7 — stepfunctions.create-state-machine-state-name-over-80-characters-rejected (81-character state name):
  expected "Invalid State Machine Definition: 'INVALID_STATE_NAME: Invalid State name: State '<81-char name>' exceeds the 80-character limit allowed by the service at /States/<81-char name>'"
  got      "Invalid State Machine Definition: 'INVALID_STATE_NAME: a state name may carry at most 80 characters, and this definition declares one of 81 at /States/<name truncated>…'"
  (fixed text: "Invalid State name: State '...' exceeds the 80-character limit allowed by the service"; the only variable part is the state name, which must appear in full in both the sentence and the pointer)

Cause 8 — stepfunctions.update-state-machine-succeed-with-next-rejected-keeps-definition:
  expected "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Field 'Next' is not supported at /States/A'"
  got      "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Field 'Next' is not supported at /States/A, SCHEMA_VALIDATION_FAILED: state 'A' is a 'Succeed' state, which ends an execution and must not carry 'Next' at /States/A/Next'"

Cause 9 — stepfunctions.update-state-machine-unknown-state-type-rejected-keeps-definition (Type 'Bogus'):
  expected "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: The field 'Type' should have one of these values: [Task, Wait, Pass, Succeed, Fail, Choice, Parallel, Map] at /States/A/Type'"
  got      "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: state 'A' is of type 'Bogus', which is not one of the state types the Amazon States Language defines (Choice, Fail, Map, Parallel, Pass, Succeed, Task, Wait) at /States/A/Type'"

Cause 10 — stepfunctions.update-state-machine-wait-seconds-and-timestamp-together-rejected:
  AssertionError: expected the call to be refused, it succeeded with {'updateDate': <ts>, 'revisionId': '<uuid>', ... 'HTTPStatusCode': 200}
  expected InvalidDefinition (400) / "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: There should only be one of the following fields: [SecondsPath, Seconds, TimestampPath, Timestamp] at /States/W'" and an unchanged stored definition; got 200, a new revisionId, and the rejected definition stored.

Model references used: com.amazonaws.sfn#CreateStateMachine (errors include InvalidDefinition, client, httpError 400; required input `definition` -> Definition @length 1..1048576), com.amazonaws.sfn#UpdateStateMachine (same InvalidDefinition error; output revisionId is produced only by a successful update), com.amazonaws.sfn#StateName (@length 1..80), com.amazonaws.sfn#ValidateStateMachineDefinitionDiagnostic (SCHEMA_VALIDATION_FAILED = 'Schema validation reported errors'; location 'could be in the format /States/<StateName>/<FieldName>').

## Steps to Reproduce
Boot the emulator from this checkout and replay the behavioral suite (simparity-behavioral-aws-v1), then re-run exactly the ten checks in trace_ids. Per cause:

1. CreateStateMachine with {"StartAt":"C","States":{"C":{"Type":"Choice","Choices":[{"Variable":"$.x","BooleanEquals":true}],"Default":"No"},"No":{"Type":"Pass","Result":"no","End":true}}} — read the HTTP status, error code and message, then ListStateMachines to confirm nothing was created.
2. CreateStateMachine with a definition whose top-level "Version" is "2.0" — read status/code/message.
3. CreateStateMachine with a DISTRIBUTED Map state M carrying "ToleratedFailurePercentage": 101 — read status/code/message.
4. CreateStateMachine with a Pass state A carrying both "Next" and "End": true — read status/code/message.
5. CreateActivity, then CreateStateMachine with a Task state T whose Resource is that activity ARN and "TimeoutSeconds": 0 — read status/code/message.
6. CreateStateMachine with a Parallel state P whose Branches[0] state X has "Next": "D" naming a state outside the branch — compare the InvalidDefinition message, specifically the MISSING_END_STATE location.
7. CreateStateMachine with a state whose name is 81 characters — compare the InvalidDefinition message and its /States/<name> pointer.
8. Create a valid state machine, then UpdateStateMachine with {"StartAt":"A","States":{"A":{"Type":"Succeed","Next":"B"},"B":{"Type":"Pass","End":true}}} — compare the full message.
9. Create a valid state machine, then UpdateStateMachine with a state A of "Type":"Bogus" — compare the full message.
10. Create a state machine whose Wait state W has only Seconds, then UpdateStateMachine with W carrying both "Seconds" and "Timestamp" — read the status and error, then DescribeStateMachine and check that `definition` is still the original and that no new revisionId was minted.
