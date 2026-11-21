Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: stepfunctions.create-express-state-machine-sqs-wait-for-task-token-rejected stepfunctions.create-state-machine-invalid-definition-did-you-mean-hint stepfunctions.jsonata-variable-scope-nested-map-parallel-boundaries stepfunctions.test-state-rejects-invalid-arns-and-unknown-integration stepfunctions.alias-routing-config-length-and-weight-bound-rejected stepfunctions.sfn-static-variable-validation-field-and-syntax-rejections stepfunctions.sfn-undefined-variable-is-error-not-warning-in-jsonpath-fields stepfunctions.jsonata-malformed-string-and-arguments-schema-validation-messages stepfunctions.jsonata-fail-error-and-cause-each-get-their-own-diagnostic
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: stepfunctions.create-express-state-machine-sqs-wait-for-task-token-rejected|run#14|AssertionError|assert express_err == expected, (
Issue-Key: stepfunctions.create-state-machine-invalid-definition-did-you-mean-hint|_error#4|AssertionError|raise AssertionError(f"expected the call to be refused, it succeeded with {result!r}")
Issue-Key: stepfunctions.jsonata-variable-scope-nested-map-parallel-boundaries|_create_error#3|AssertionError|raise AssertionError(f"CreateStateMachine accepted a definition it rejects: {arn!r}")
Issue-Key: stepfunctions.test-state-rejects-invalid-arns-and-unknown-integration|run#10|AssertionError|assert code == "InvalidDefinition", f"expected InvalidDefinition, got {code}: {message!r}"
Issue-Key: stepfunctions.alias-routing-config-length-and-weight-bound-rejected|run#9|AssertionError|assert err["Message"] == expected, f"CreateStateMachineAlias {label}: expected {expected!r}, got {err['Message']!r}"
Issue-Key: stepfunctions.sfn-static-variable-validation-field-and-syntax-rejections|_only_diagnostic#2|AssertionError|assert resp["result"] == "FAIL", f"expected FAIL, got {resp!r}"
Issue-Key: stepfunctions.sfn-undefined-variable-is-error-not-warning-in-jsonpath-fields|_undefined_variable_tuples#6|AssertionError|assert message.startswith(prefix) and suffix in message, (
Issue-Key: stepfunctions.jsonata-malformed-string-and-arguments-schema-validation-messages|_diag_fail#5|AssertionError|assert v["result"] == "FAIL", f"ValidateStateMachineDefinition result {v['result']!r}: {v!r}"
Issue-Key: stepfunctions.jsonata-fail-error-and-cause-each-get-their-own-diagnostic|run#6|AssertionError|assert diags == [("ERROR", "SCHEMA_VALIDATION_FAILED", "/States/F/Error", _UNCLOSED)], (

# stepfunctions: what is left of the definition-validation and validation-clause report — six causes that still diverge

## Operation
CreateStateMachine

## Description
This is the remainder of
`issues/things-that-now-work/20261110T010311Z-stepfunctions-10-net-new-causes-createstatemachine-updatestatemachine-alias.md`,
recorded on its own so none of it is hidden by that report being closed. Of its ten
causes, five are closed outright (out-of-range figures in a definition, an invalid
routing configuration, the update that names only an encryption configuration, the
version-description sentence, the role-ARN sentence) and three of the fourth cause's
four renderings are; what follows is every unit that still diverges. Each is
independent.

Cause 1 of 6: the EXPRESS `.waitForTaskToken` refusal does not match the recorded one.
AWS: `CreateStateMachine` of an EXPRESS state machine whose `Task` asks for the
callback pattern is refused `InvalidDefinition` with
`Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Express state machine does not support '.waitForTaskToken' service integration at /States/<state>/Resource'`.
Emulator: it refuses with exactly that code, location and sentence (the `.sync` unit
of the same family now replays clean), so the recorded refusal differs in something
the report does not quote — a second subject the recorded definition also earns, or
another member of the recorded response.
Fix: compare the whole recorded refusal with what the definition that unit submits
earns, and close the difference.
Traces: stepfunctions.create-express-state-machine-sqs-wait-for-task-token-rejected

Cause 2 of 6: a near-miss (misspelled) definition field is still accepted.
AWS: a definition carrying a field whose name is a near miss of a known field is
refused `InvalidDefinition`, naming the unknown field and suggesting the intended one.
Emulator: the rule is implemented — every object the language declares a closed field
set for is judged against it, and the refusal suggests the field a name is one edit
from (`crate::state::asl::unsupported_field_message`) — but the recorded definition is
accepted, so its misspelled field sits at a position the closed sets do not cover. The
report does not quote that definition.
Fix: find the position the recorded field sits at and judge that object against the
set the language declares for it.
Traces: stepfunctions.create-state-machine-invalid-definition-did-you-mean-hint

Cause 3 of 6: the nested Map/Parallel variable-scope definition is still created.
AWS: the definition is refused at `CreateStateMachine`.
Emulator: the create-time analysis now reads the one scope boundary the run reads (a
`Map` processor in `DISTRIBUTED` mode inherits no variables, as a child workflow
execution), and a JSONata read of a variable the scope may not hold stays the warning
a recorded suite pins (`tests/all/jsonata_variable_scope.rs`: such a definition IS
created and fails at run time with `States.QueryEvaluationError`). The recorded
definition is still created, so what AWS refuses it for is something else in it.
Fix: identify the rule the recorded definition breaks.
Traces: stepfunctions.jsonata-variable-scope-nested-map-parallel-boundaries

Cause 4 of 6: `TestState` answers a `RoleArn` refusal for a definition AWS refuses.
AWS: `TestState` over a `Task` state whose `Resource` names an integration the service
does not define is refused `InvalidDefinition`.
Emulator: `ValidationException` `RoleArn must be specified when testing a Task state` —
the definition is never judged for the `Resource` it names, and the missing-role
precondition is reached first.
Fix: a `Task` state's `Resource` is judged where the definition is (the documented
`INVALID_RESOURCE` diagnostic), before the members the request owes.
Traces: stepfunctions.test-state-rejects-invalid-arns-and-unknown-integration

Cause 5 of 6: a non-empty list member's validation clause states no value.
AWS: a `routingConfiguration` past its `@length` maximum is refused
`1 validation error detected: Value '[RoutingConfigurationListItem(stateMachineVersionArn=<arn>, weight=34), …]' at 'routingConfiguration' failed to satisfy constraint: Member must have length less than or equal to 2`
— the list rendered as the SDK prints its elements.
Emulator: the clause states no value for it. The EMPTY list is rendered (`Value '[]'`),
which is the one a layer holding only the violations can determine: `override_refusal`
is handed the violations and never the request, and the framework replaces a collection
with a `<n> items` summary.
Fix: the per-operation echo-collecting wrapper
(`service_common::validation::refusing_violations_with_declared_echoes_over`, which
renders a structure exactly that way), as `service-com-amazonaws-eventbridge-awsevents`
wires it, so the clause can quote the collection the request carried.
Traces: stepfunctions.alias-routing-config-length-and-weight-bound-rejected

Cause 6 of 6: two of `ValidateStateMachineDefinition`'s diagnostic classes are missing,
and two more diverge in their wording.
AWS: `result` is `FAIL` for a definition whose static variable validation fails (a bad
field or syntax) and for one carrying a malformed JSONata string or an `Arguments`
schema violation; the `UNDEFINED_VARIABLE` message for a `*Path` member has a shape of
its own (the reference-field form `The variable '$<name>' is not defined.` is accepted
for a payload template's `.$` member and not for `SecondsPath`); and the diagnostic for
an unclosed `{%` has AWS's own sentence.
Emulator: `{'result': 'OK', 'diagnostics': []}` for the first two definitions, which
the report does not quote; one sentence for the undefined-variable diagnostic whatever
field names it; and this service's own sentence for an expression that is not the whole
value of its field.
Fix: the diagnostics engine behind `ValidateStateMachineDefinition`
(`crate::state::asl::diagnose`), over the recorded definitions and sentences.
Traces: stepfunctions.sfn-static-variable-validation-field-and-syntax-rejections,
stepfunctions.sfn-undefined-variable-is-error-not-warning-in-jsonpath-fields,
stepfunctions.jsonata-malformed-string-and-arguments-schema-validation-messages,
stepfunctions.jsonata-fail-error-and-cause-each-get-their-own-diagnostic

## Evidence
1) `expected {'Code': 'InvalidDefinition', 'Status': 400, 'Message': "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Express state machine does not support '.waitForTaskToken' service integration …"}`.
2) `expected the call to be refused, it succeeded with {'stateMachineArn': '…sim2real-behav-sm-near-…'}`.
3) `CreateStateMachine accepted a definition it rejects: 'arn:aws:states:…:stateMachine:…sfnvarscope-sm-…'`.
4) `expected InvalidDefinition, got ValidationException: 'RoleArn must be specified when testing a Task state'`.
5) `CreateStateMachineAlias 3 entries: expected "1 validation error detected: Value '[RoutingConfigurationListItem(stateMachineVersionArn=…:1, weight=34), RoutingConfigurationListItem(…)]' …"`.
6) `expected FAIL, got {'result': 'OK', 'diagnostics': [], 'truncated': False}`; `UNDEFINED_VARIABLE message has an unexpected shape: "The variable '$waitSeconds' is not defined."`; `a bad Error alone must produce exactly one diagnostic naming /States/F/Error`.

## Steps to Reproduce
1. CreateStateMachine type=EXPRESS with an `sqs:sendMessage.waitForTaskToken` Task and compare the whole refusal.
2. CreateStateMachine with the recorded misspelled field.
3. CreateStateMachine with the recorded nested Map/Parallel variable-scope definition.
4. TestState with a `Task` state naming an integration the service does not define, and no roleArn.
5. CreateStateMachineAlias with three routingConfiguration entries.
6. ValidateStateMachineDefinition on the recorded static-variable, malformed-JSONata and Fail-state definitions, and on a `SecondsPath` naming an unassigned variable.
