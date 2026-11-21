Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: stepfunctions.create-state-machine-already-exists-publish-state-mismatch stepfunctions.create-state-machine-map-tolerated-failure-percentage-over-100-rejected stepfunctions.start-sync-execution-nonexistent-version-and-alias-rejected stepfunctions.start-sync-execution-on-deleted-version-and-alias-arn-does-not-exist stepfunctions.list-executions-filtered-by-version-and-alias-arn stepfunctions.test-state-assign-rejected-on-succeed-and-fail stepfunctions.test-state-schema-validation-rejects-unsupported-fields stepfunctions.log-sfn-sync-child-failure-cause-redacts-input-but-keeps-failed-status
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: stepfunctions.create-state-machine-already-exists-publish-state-mismatch|run#11|AssertionError|assert message == (
Issue-Key: stepfunctions.create-state-machine-map-tolerated-failure-percentage-over-100-rejected|run#8|AssertionError|assert message == _EXPECTED, f"expected {_EXPECTED!r}, got {message!r}"
Issue-Key: stepfunctions.start-sync-execution-nonexistent-version-and-alias-rejected|run#17|AssertionError|assert version_error.get("Message") == expected_version_message, (
Issue-Key: stepfunctions.start-sync-execution-on-deleted-version-and-alias-arn-does-not-exist|run#16|AssertionError|assert version_error.get("Message") == expected_version_message, (
Issue-Key: stepfunctions|ListExecutions|StateMachineDoesNotExist
Issue-Key: stepfunctions.test-state-assign-rejected-on-succeed-and-fail|run#5|AssertionError|assert message == _EXPECTED_MESSAGE, f"Succeed+Assign: unexpected message {message!r}"
Issue-Key: stepfunctions.test-state-schema-validation-rejects-unsupported-fields|run#7|AssertionError|assert message == (
Issue-Key: stepfunctions.log-sfn-sync-child-failure-cause-redacts-input-but-keeps-failed-status|run#21|KeyError|got_input = json.loads(cause["Input"])
Issue-Commit: 1945f04af551ab9122ba85c8ecd7b899fcb2daf4

# stepfunctions: 6 net-new causes — StateMachineAlreadyExists and TestState InvalidDefinition messages, the 100.0 bound, qualified-ARN not-exist wording, ListExecutions on a never-created qualified ARN, and the .sync child's logged cause Input

> _Note: this issue was identified against the code base at commit `1945f04af551ab9122ba85c8ecd7b899fcb2daf4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStateMachine

## Description
Six independent, net-new root causes on the Step Functions control plane and its logged execution data. None is covered by an open issue; where an open issue is adjacent it is named in the cause.

Cause 1 of 6: CreateStateMachine's StateMachineAlreadyExists message does not say WHICH mismatch caused it.
AWS: Smithy: CreateStateMachine error StateMachineAlreadyExists, client error, HTTP 400. When a name already exists and the repeat differs only in publish state (or, under publish=true, in versionDescription), the message is one of three fixed sentences and carries NO request-specific value at all:
  - created with publish=true, repeated with publish=false -> "State Machine Already Exists: This state machine was already created with a published version."
  - created with publish=false, repeated with publish=true -> "State Machine Already Exists: This state machine was already created without a published version. Please use PublishStateMachineVersion to publish a version."
  - created with publish=true, repeated with publish=true and a different versionDescription -> "State Machine Already Exists: This state machine was already created with a published version using a different description."
The prefix "State Machine Already Exists: " is fixed and so is each explanatory sentence; nothing is interpolated. The rejected repeat must create no additional version, and a truly identical repeat (same publish, same versionDescription) is idempotent and returns the same stateMachineArn and stateMachineVersionArn.
Emulator: the code is right (StateMachineAlreadyExists) but the message is the generic ARN form, "State Machine Already Exists: '<stateMachineArn>'" — the ARN of the existing machine is interpolated where AWS states the reason.
Why: a client cannot tell a plain duplicate-name collision from a publish-state or description mismatch, so it cannot know to call PublishStateMachineVersion instead.
Fix: the CreateStateMachine handler's already-exists branch — compare publish and versionDescription against the stored machine and emit the matching sentence.
Traces: stepfunctions.create-state-machine-already-exists-publish-state-mismatch

Cause 2 of 6: a schema maximum on a floating-point definition field is rendered as an integer.
AWS: Smithy: CreateStateMachine error InvalidDefinition, 400. For a DISTRIBUTED Map with ToleratedFailurePercentage 101 the message is "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Maximum value is 100.0 at /States/<state>/ToleratedFailurePercentage'" — the bound is rendered as the floating-point value the field's type carries (100.0), and the path is taken from the definition. The boundary value 100 is accepted and stored byte-for-byte as written, and the refused create leaves no state machine of that name.
Emulator: the same diagnostic but "Maximum value is 100" — the bound is rendered as an integer.
Why: a client asserting on, or parsing, the documented diagnostic does not match; the rendering also mis-states the field's type (it is a percentage with a fractional part, so 100.5 is as invalid as 101).
Fix: the ASL schema-validation diagnostic for numeric range bounds — render the bound in the field's own numeric type.
Traces: stepfunctions.create-state-machine-map-tolerated-failure-percentage-over-100-rejected

Cause 3 of 6: a version ARN that does not exist is reported with the emulator's own 'Version' wording.
AWS: Smithy: StartSyncExecution error StateMachineDoesNotExist, client error, HTTP 400. For a stateMachineArn naming a version that was never published, and for one naming a version that was published and then deleted, the message is "State Machine Does Not Exist: '<the stateMachineArn exactly as supplied>'" — the prefix is fixed text, the ARN is the request's own value. There is no separate 'version' phrasing.
Emulator: "State Machine Version Does Not Exist: '<arn>'" — an extra word in the fixed text. The code (StateMachineDoesNotExist) and status are right.
Why: a client matching the documented message (or aggregating on it) does not match; the same wrong sentence appears wherever a qualified ARN is resolved.
Fix: the not-found message for a qualified (version) ARN, in the shared state-machine resolution path used by StartSyncExecution and its siblings.
Traces: stepfunctions.start-sync-execution-nonexistent-version-and-alias-rejected, stepfunctions.start-sync-execution-on-deleted-version-and-alias-arn-does-not-exist

Cause 4 of 6: ListExecutions raises on a never-created qualified ARN instead of returning an empty list.
AWS: Smithy: ListExecutions output.executions is required (a list) and the operation's errors include StateMachineDoesNotExist (400) — but a qualified ARN (a version ARN such as '<stateMachineArn>:999', or an alias ARN that was never created) that resolves to nothing is NOT an error: the call returns 200 with executions == []. Executions started through a version or alias also stay listed by that ARN after the version or alias is deleted.
Emulator: 400 StateMachineDoesNotExist "State Machine Version Does Not Exist: '<stateMachineArn>:999'" — the listing refuses rather than answering empty, which also aborts the rest of that sequence (the PENDING_REDRIVE ValidationException clause and the listed-after-delete behaviour were never reached).
Why: a client polling a qualified ARN for executions (a common deploy-time check) gets a hard 400 it must special-case, instead of the empty page AWS returns.
Fix: the ListExecutions handler's ARN resolution — an unresolvable qualified ARN yields an empty result set, not StateMachineDoesNotExist.
Traces: stepfunctions.list-executions-filtered-by-version-and-alias-arn

Cause 5 of 6: TestState's InvalidDefinition messages are the bare diagnostic, not the wrapped definition message.
AWS: Smithy: TestState errors include InvalidDefinition, client error, HTTP 400. The message has the same wrapped shape CreateStateMachine uses, with the synthetic state name TestState assigns: "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Field '<FieldName>' is not supported at /States/StateName'" — for an Assign field on a Succeed or a Fail state ('Assign'), and for the Task-only field Resource on a Pass state ('Resource'). The fixed text is the wrapper and the "Field '<x>' is not supported at /States/StateName" sentence; only the field name varies. A state object with no Type field at all is refused with the distinct message "Missing required field: 'Type'". The same shapes without the offending field succeed (status SUCCEEDED / FAILED respectively).
Emulator: the code is InvalidDefinition but the message is the unwrapped diagnostic with no location: "SCHEMA_VALIDATION_FAILED: Field 'Assign' is not supported" and "SCHEMA_VALIDATION_FAILED: Field 'Resource' is not supported".
Why: a client that parses TestState refusals the same way it parses CreateStateMachine refusals (the wrapper plus the /States/... location) cannot, and loses the location entirely.
Fix: TestState's definition-validation error path — wrap the diagnostic in the "Invalid State Machine Definition: '<diagnostic> at /States/StateName'" form. Adjacent but different: open issue 20261121T143000Z-stepfunctions-definition-validation-and-list-clause-remainder cause 4 covers TestState answering a RoleArn ValidationException before judging a Task's Resource; this cause is the message shape of the InvalidDefinition it does return.
Traces: stepfunctions.test-state-assign-rejected-on-succeed-and-fail, stepfunctions.test-state-schema-validation-rejects-unsupported-fields

Cause 6 of 6: the logged cause of a failed .sync:2 child omits Input/InputDetails even when includeExecutionData is true.
AWS: recorded behavior only (loggingConfiguration documented at cw-logs.html; Smithy: CreateStateMachine.loggingConfiguration -> LoggingConfiguration.includeExecutionData). When a parent's Task uses arn:aws:states:::states:startExecution.sync:2 and the child FAILS, Step Functions auto-generates the TaskFailed/ExecutionFailed cause as a JSON object. Under includeExecutionData=true that object carries the child's own input verbatim under "Input" (a JSON string) together with "InputDetails": {"Included": true}, alongside "Status": "FAILED" and the child's "Error"/"Cause". Under includeExecutionData=false the same object keeps Status/Error/Cause and drops "Input" and "InputDetails" entirely. The member names are fixed; the input value comes from the child execution.
Emulator: the include=true log's cause object has no "Input" member at all (reading cause["Input"] raises KeyError), so the two logging modes are indistinguishable and InputDetails.Included is never reported.
Why: a client that reads the log stream to recover a failed child's input (the documented reason to switch includeExecutionData on) gets nothing, and cannot verify that redaction is actually configured either way.
Fix: the auto-generated .sync:2 child-failure cause in the execution-logging emitter — populate Input/InputDetails when includeExecutionData is true and omit both when it is false.
Traces: stepfunctions.log-sfn-sync-child-failure-cause-redacts-input-but-keeps-failed-status

## Evidence
Cause 1: expected "State Machine Already Exists: This state machine was already created with a published version.", got "State Machine Already Exists: 'arn:aws:states:<region>:<account>:stateMachine:<name>'" (the ARN interpolated where AWS states the reason).

Cause 2: AssertionError: expected "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Maximum value is 100.0 at /States/M/ToleratedFailurePercentage'", got "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Maximum value is 100 at /States/M/ToleratedFailurePercentage'".

Cause 3: AssertionError: expected message "State Machine Does Not Exist: '<stateMachineArn>:999'", got "State Machine Version Does Not Exist: '<stateMachineArn>:999'"; and for a deleted version, expected "State Machine Does Not Exist: '<stateMachineArn>:1'", got "State Machine Version Does Not Exist: '<stateMachineArn>:1'".

Cause 4: botocore.errorfactory.StateMachineDoesNotExist: An error occurred (StateMachineDoesNotExist) when calling the ListExecutions operation: State Machine Version Does Not Exist: '<stateMachineArn>:999' — expected HTTP 200 with executions == [].

Cause 5: AssertionError: Succeed+Assign: unexpected message "SCHEMA_VALIDATION_FAILED: Field 'Assign' is not supported" (expected "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Field 'Assign' is not supported at /States/StateName'"); and unexpected SCHEMA_VALIDATION_FAILED message: "SCHEMA_VALIDATION_FAILED: Field 'Resource' is not supported" (expected the same wrapped form naming 'Resource').

Cause 6: KeyError: 'Input' at got_input = json.loads(cause["Input"]) — the includeExecutionData=true TaskFailed/ExecutionFailed cause object carries no Input member (and so no InputDetails {"Included": true}).

## Steps to Reproduce
1. (Cause 1) CreateStateMachine name=N publish=true versionDescription="v1"; repeat with publish=false -> expect the "already created with a published version." sentence. CreateStateMachine name=M publish=false; repeat with publish=true versionDescription="v1" -> expect the "without a published version. Please use PublishStateMachineVersion..." sentence. CreateStateMachine name=P publish=true versionDescription="v1"; repeat with publish=true versionDescription="v2-different" -> expect the "using a different description." sentence. After each refusal, ListStateMachineVersions must show no extra version; repeating identically must be idempotent.
2. (Cause 2) CreateStateMachine with a DISTRIBUTED Map and ToleratedFailurePercentage 100 (accepted, stored verbatim), then 101 (refused) and compare the InvalidDefinition message.
3. (Cause 3) Publish a version, then StartSyncExecution with stateMachineArn '<arn>:999'; and publish, delete the version, StartSyncExecution with '<arn>:1'. Compare Error.Message.
4. (Cause 4) ListExecutions(stateMachineArn='<arn>:999') and ListExecutions with a never-created alias ARN -> both must return executions == []. Then ListExecutions(stateMachineArn=<version arn>, statusFilter='PENDING_REDRIVE') -> ValidationException "StatusFilter of PENDING_REDRIVE cannot be applied to ListExecutions call with StateMachine ARN". Then delete the version/alias and confirm their executions are still listed by those ARNs.
5. (Cause 5) TestState(definition={"QueryLanguage":"JSONata","Type":"Succeed","Assign":{"x":1}}, input="{}"); same with Type=Fail plus Error/Cause; TestState(definition={"Type":"Pass","Resource":"<lambda arn>","End":true}); TestState(definition={"End":true}) -> "Missing required field: 'Type'". Compare each Error.Message.
6. (Cause 6) CreateStateMachine two parents over the same child, one with loggingConfiguration includeExecutionData=true and one false, each with a Task Resource arn:aws:states:::states:startExecution.sync:2; run both so the child FAILS; read the delivered TaskFailed and ExecutionFailed log events and compare the parsed details.cause objects.
