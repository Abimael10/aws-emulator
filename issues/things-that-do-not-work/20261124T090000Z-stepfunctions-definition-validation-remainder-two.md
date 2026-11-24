Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: stepfunctions.create-express-state-machine-sqs-wait-for-task-token-rejected stepfunctions.create-state-machine-invalid-definition-did-you-mean-hint stepfunctions.jsonata-variable-scope-nested-map-parallel-boundaries stepfunctions.test-state-rejects-invalid-arns-and-unknown-integration stepfunctions.sfn-static-variable-validation-field-and-syntax-rejections stepfunctions.sfn-undefined-variable-is-error-not-warning-in-jsonpath-fields stepfunctions.jsonata-malformed-string-and-arguments-schema-validation-messages stepfunctions.jsonata-fail-error-and-cause-each-get-their-own-diagnostic
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: stepfunctions.create-express-state-machine-sqs-wait-for-task-token-rejected|run#14|AssertionError|assert express_err == expected, (
Issue-Key: stepfunctions.create-state-machine-invalid-definition-did-you-mean-hint|_error#4|AssertionError|raise AssertionError(f"expected the call to be refused, it succeeded with {result!r}")
Issue-Key: stepfunctions.jsonata-variable-scope-nested-map-parallel-boundaries|_create_error#3|AssertionError|raise AssertionError(f"CreateStateMachine accepted a definition it rejects: {arn!r}")
Issue-Key: stepfunctions.test-state-rejects-invalid-arns-and-unknown-integration|run#10|AssertionError|assert code == "InvalidDefinition", f"expected InvalidDefinition, got {code}: {message!r}"
Issue-Key: stepfunctions.sfn-static-variable-validation-field-and-syntax-rejections|_only_diagnostic#2|AssertionError|assert resp["result"] == "FAIL", f"expected FAIL, got {resp!r}"
Issue-Key: stepfunctions.sfn-undefined-variable-is-error-not-warning-in-jsonpath-fields|_undefined_variable_tuples#6|AssertionError|assert message.startswith(prefix) and suffix in message, (
Issue-Key: stepfunctions.jsonata-malformed-string-and-arguments-schema-validation-messages|run#5|AssertionError|assert _diag_fail(sfn, plain_string) == [
Issue-Key: stepfunctions.jsonata-fail-error-and-cause-each-get-their-own-diagnostic|run#6|AssertionError|assert diags == [("ERROR", "SCHEMA_VALIDATION_FAILED", "/States/F/Error", _UNCLOSED)], (

# stepfunctions: the definition-validation remainder — five causes whose recorded definitions and sentences the reports do not quote

## Description
This is the remainder of
`issues/things-that-now-work/20261121T143000Z-stepfunctions-definition-validation-and-list-clause-remainder.md`,
recorded on its own so none of it is hidden by that report being closed. Of its six
causes, cause 5 is closed outright (a clause about a non-empty list member quotes the
collection the request carried) and two of cause 6's four classes have their rule
implemented; what follows is every unit that still diverges. Each is independent, and
each one's remaining work needs a value the reports do not carry — the recorded
definition, or the recorded sentence — so a fixer should start by reading the recorded
unit rather than by re-deriving the rule.

Cause 1 of 5: the EXPRESS `.waitForTaskToken` refusal still differs from the recorded
one. AWS: `CreateStateMachine` of an EXPRESS state machine whose `Task` asks for the
callback pattern is refused `InvalidDefinition` with
`Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Express state machine does not support '.waitForTaskToken' service integration at /States/<state>/Resource'`.
Emulator: that code, that status and exactly that message, for the bare `Task`, for one
carrying the queue parameters and the task token, for one carrying both windows, for one
whose parameters carry no token, and for a second state after it — every shape of the
definition probed produces the one subject and nothing else
(`tests/all/definition_static_rules.rs` pins the rule over four integrations). So the
recorded refusal earns a SECOND subject the report does not quote, for something in the
recorded definition this walk does not judge at all.
Fix: read the recorded definition and the whole recorded message, and close the
difference.
Traces: stepfunctions.create-express-state-machine-sqs-wait-for-task-token-rejected

Cause 2 of 5: the near-miss (misspelled) definition field of the recorded definition is
still accepted. AWS: a definition carrying a field whose name is a near miss of a known
field is refused `InvalidDefinition`, naming the unknown field and suggesting the
intended one. Emulator: every object position the Amazon States Language declares a
closed field set for is now judged against it and suggests out of that set — the state
machine object, a state, a retrier, a catcher, a `Choice` rule and the rules nested in
one, the four objects a `Map` state carries beside its processor, the two configurations
inside them, and a `Task` state's `Credentials`
(`tests/all/definition_declared_objects.rs` is the table) — and the recorded definition
is still created. The positions left are the ones whose field set is NOT the language's
but an INTEGRATION's: the `Parameters` of an optimized integration, whose admitted
fields are the invoked API's own input members. That is the position to check the
recorded field against; judging it needs the invoked operation's input members, which
this service does not carry today (it hands a task's parameters to the target service,
which validates them when the call is made).
Traces: stepfunctions.create-state-machine-invalid-definition-did-you-mean-hint

Cause 3 of 5: the nested Map/Parallel variable-scope definition is still created.
AWS: the definition is refused at `CreateStateMachine`.
Emulator: the create-time analysis reads the scope boundary the run reads (a `Map`
processor in `DISTRIBUTED` mode inherits no variables), a JSONata read of a variable the
scope may not hold stays the warning a recorded suite pins
(`tests/all/jsonata_variable_scope.rs`: such a definition IS created and fails at run
time with `States.QueryEvaluationError`), and the fields of a `Choice` rule now name
variables the language resolves, which is an error. The recorded definition is still
created, and what AWS refuses it for is something else in it; the report quotes neither
the definition nor the refusal.
Fix: read the recorded definition and identify the rule it breaks.
Traces: stepfunctions.jsonata-variable-scope-nested-map-parallel-boundaries

Cause 4 of 5: `TestState` answers a `RoleArn` refusal for a definition AWS refuses.
AWS: `TestState` over a `Task` state whose `Resource` names an integration the service
does not define is refused `InvalidDefinition` (the model documents the
`INVALID_RESOURCE` diagnostic: "The value of a Task-state resource field is invalid").
Emulator: `ValidationException` `RoleArn must be specified when testing a Task state` —
the definition IS judged first (`asl::validate_single_state_with`, before the
missing-role precondition) but nothing in that walk judges the `Resource` it names.
What makes this cause a decision rather than an omission: this emulator deliberately
reports a `Resource` naming work it makes no call for at RUN time, with the state's own
failure, rather than refusing the definition — `tests/all/test_state.rs` ("a Task state
whose Resource names no call is not run"),
`tests/all/unmade_integration_status_change_error.rs` (a service with no wire facts, an
operation whose bindings are not carried, an unknown integration pattern, a bare
resource ARN, a `Resource` that is no ARN) and `tests/all/s3_sdk_task_integration.rs`
("the optimized family, which S3 publishes no integration for") all pin that, and the
last of them is AWS's own behaviour for a service AWS publishes no optimized integration
for. So a catalog check at create time would refuse the integrations AWS defines and
this emulator does not serve, and a FORM check (an ARN of one of the forms the language
defines for a task) refuses the four rows above instead.
Fix: read the recorded `Resource` to see which of the two the recorded refusal is about,
then judge that in the definition walk and restate the rows above.
Traces: stepfunctions.test-state-rejects-invalid-arns-and-unknown-integration

Cause 5 of 5: three of `ValidateStateMachineDefinition`'s diagnostics still diverge.
AWS: `result` is `FAIL` for a definition whose static variable validation fails (a bad
field or syntax); the `UNDEFINED_VARIABLE` message for a `*Path` member has a shape of
its own (the reference-field form `The variable '$<name>' is not defined.` is accepted
for a payload template's `.$` member and not for `SecondsPath`); and the diagnostic for
a field whose value is no JSONata expression has AWS's own sentence.
Emulator: `{'result': 'OK', 'diagnostics': []}` for the static-variable definition,
which the report does not quote; one sentence for the undefined-variable diagnostic
whatever field names it; and this service's own sentence for a value that is no JSONata
expression (`'<text>' is not a JSONata expression: an expression is the whole value of
the field, opened with '{%' and closed with '%}'`), which every such value — a text
carrying the marks without being one expression, and a literal string at a member whose
value the language evaluates — is now stated by. The severity, the code and the location
of each of those diagnostics already match the recorded ones; only the sentences do not,
and no report at hand quotes the recorded ones, so no wording was derived for them.
Fix: read the recorded sentences, and the recorded static-variable definition, off the
recorded units.
Traces: stepfunctions.sfn-static-variable-validation-field-and-syntax-rejections,
stepfunctions.sfn-undefined-variable-is-error-not-warning-in-jsonpath-fields,
stepfunctions.jsonata-malformed-string-and-arguments-schema-validation-messages,
stepfunctions.jsonata-fail-error-and-cause-each-get-their-own-diagnostic

## Evidence
1) `expected {'Code': 'InvalidDefinition', 'Status': 400, 'Message': "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Express state machine does not support '.waitForTaskToken' service integrati…"}`.
2) `expected the call to be refused, it succeeded with {'stateMachineArn': '…sim2real-behav-sm-near-…'}`.
3) `CreateStateMachine accepted a definition it rejects: 'arn:aws:states:…:stateMachine:…sfnvarscope-sm-…'`.
4) `expected InvalidDefinition, got ValidationException: 'RoleArn must be specified when testing a Task state'`.
5) `expected FAIL, got {'result': 'OK', 'diagnostics': [], 'truncated': False}`; `UNDEFINED_VARIABLE message has an unexpected shape: "The variable '$waitSeconds' is not defined."`; `plain-string Condition diagnostics: [('ERROR', 'SCHEMA_VALIDATION_FAILED', '/States/Route/Choices[0]/Condition', …)]`; `a bad Error alone must produce exactly one diagnostic naming /States/F/Error`.

## Steps to Reproduce
1. CreateStateMachine type=EXPRESS with the recorded `sqs:sendMessage.waitForTaskToken` definition and compare the whole refusal.
2. CreateStateMachine with the recorded misspelled field.
3. CreateStateMachine with the recorded nested Map/Parallel variable-scope definition.
4. TestState with the recorded `Task` state `Resource` and no roleArn.
5. ValidateStateMachineDefinition on the recorded static-variable definition, on a `SecondsPath` naming an unassigned variable, on the recorded plain-string `Condition` and `Arguments`, and on a `Fail` state whose `Error` opens an expression it never closes.
