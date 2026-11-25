Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: agent-registry-control.custom-metadata-schema-evolution-additive-only
Issue-Shape-Id: com.amazonaws.agentregistrycontrol#AgentRegistryControl
Issue-Key: agent-registry-control.custom-metadata-schema-evolution-additive-only|run#8|AssertionError|assert message == expected, f"unexpected message {message!r}"
Issue-Commit: 7c1a32607181dcc252ace77444601da30e81bd5f

# UpdateRegistry: custom-metadata schema-evolution refusals are worded in the emulator's own sentences, not AWS's

> _Note: this issue was identified against the code base at commit `7c1a32607181dcc252ace77444601da30e81bd5f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateRegistry

## Description
One root cause, one net-new cause in this issue.

Cause 1 of 1: `crate::state::schema_evolution::message()` composes the `ValidationException` message for a custom-metadata schema replacement that withdraws something, and every sentence it composes diverges from the sentence AWS words for that error class — a different lead-in, a different way of naming the scope, and no quoting of the identifiers.

AWS: `UpdateRegistry` models `ValidationException` (HTTP 400) and documents `customMetadataSchemaConfiguration` as "The new custom metadata schema configuration, which replaces the current one. You can add fields and allowed values, but you can't remove a field or an allowed value, or change a field's type." (Smithy: UpdateRegistry.customMetadataSchemaConfiguration -> UpdatedCustomMetadataSchemaConfiguration; errors include ValidationException @httpError 400). The model does not state the refusal TEXT, so the wording below is recorded behavior only, taken from the behavioral check's recorded expectations:
  * a field dropped from a scope -> `Cannot remove field '<field>' from record type '<scope>'. Schema evolution is additive-only.` Fixed text: `Cannot remove field '`, `' from record type '`, `'. Schema evolution is additive-only.` Variable parts: `<field>` is the path of the field the current schema declares and the replacement no longer does, single-quoted; `<scope>` is the scope the schema is carried in, and for the configuration's `defaultSchema` AWS names that scope `default schema` (two words, lower case, single-quoted) — NOT the member name `defaultSchema`.
  * an enumerated value dropped from a field -> `Cannot remove enum value '<value>' from field '<field>' in record type '<scope>'.` Fixed text: `Cannot remove enum value '`, `' from field '`, `' in record type '`, `'.` Variable parts: the dropped enumerated value, the field path, and the scope, each single-quoted; note AWS words this one with NO trailing "additive-only" sentence.
The recorded status/code are correct in the emulator: HTTP 400 `ValidationException` (the check's `code == "ValidationException"` assertion passed), and the refused replacement correctly leaves the stored schema untouched.

Emulator: for the field-removal case it answers, verbatim from the replay, `The custom metadata schema cannot remove field critical from defaultSchema; fields can only be added.` — a different lead-in (`The custom metadata schema cannot remove field ...`), the member name `defaultSchema` where AWS says `record type 'default schema'`, no single quotes around the field name, and a `; fields can only be added.` tail in place of `. Schema evolution is additive-only.`. The same function words the allowed-value case `The custom metadata schema cannot remove allowed value <value> of field <field> in <scope>; allowed values can only be added.`, which diverges from AWS's `Cannot remove enum value '<value>' from field '<field>' in record type '<scope>'.` the same way; the check asserts that second sentence too and only failed earlier because the field-removal assertion fires first.

Why: a client that reads the refusal — a deploy tool surfacing why a schema change was rejected, a test asserting the rejection reason, or any code that parses the field name or the dropped value back out of the sentence — gets text that does not match AWS. The single quotes AWS puts around the field name and the value are what make the identifiers extractable; the emulator leaves them unquoted, so a parser keyed on AWS's wording finds nothing and a caller is told the change failed without being told, in AWS's terms, what it withdrew.

Fix: the refusal-wording helper that this service funnels these sentences through — the `message()` composer for schema-evolution withdrawals in the agent-registry-control state module, including how it labels a scope (`Scope::label()` must render the `defaultSchema` scope as AWS's `default schema`) — not the detection logic, which picks the right withdrawal and the right field. The handler's status, error shape and its "refuse before the registry is written" ordering are already right and should not change.

Traces: agent-registry-control.custom-metadata-schema-evolution-additive-only. Relevant source: services/service-com-amazonaws-agentregistrycontrol-agentregistrycontrol/src/state/schema_evolution.rs lines 75-86 (`Scope::label`) and 242-261 (`message`); call site services/service-com-amazonaws-agentregistrycontrol-agentregistrycontrol/src/ops/update_registry.rs lines 236-250.

## Evidence
Check agent-registry-control.custom-metadata-schema-evolution-additive-only, [mismatch]:

  File "check_agent_registry_control_custom_metadata_schema_evolution_additive_only.py", line 151, in run
    assert message == expected, f"unexpected message {message!r}"
  AssertionError: unexpected message 'The custom metadata schema cannot remove field critical from defaultSchema; fields can only be added.'

The check creates a READY registry whose customMetadataSchemaConfiguration.defaultSchema declares fields `environment` (string, enum [dev, prod]) and `critical` (boolean), then calls UpdateRegistry with a replacement defaultSchema that drops `critical`.

Expected (AWS, recorded): HTTP 400, Error.Code `ValidationException`, Error.Message
  Cannot remove field '<dropped field>' from record type 'default schema'. Schema evolution is additive-only.
Actual (emulator): HTTP 400, Error.Code `ValidationException` (correct), Error.Message
  The custom metadata schema cannot remove field <dropped field> from defaultSchema; fields can only be added.

Second case in the same check, not reached because the first assertion fails, same composer:
Expected: Cannot remove enum value '<dropped value>' from field '<field>' in record type 'default schema'.
Emulator composes: The custom metadata schema cannot remove allowed value <dropped value> of field <field> in defaultSchema; allowed values can only be added.

Not diverging, and should stay as it is: the error code (ValidationException), the 400 status, and GetRegistry still reporting the original stored schema after each refusal.

## Steps to Reproduce
1. CreateRegistry with customMetadataSchemaConfiguration.defaultSchema = {"type":"object","properties":{"environment":{"type":"string","enum":["dev","prod"]},"critical":{"type":"boolean"}},"required":["environment"]} and wait for status READY.
2. UpdateRegistry(registryId=<id>, customMetadataSchemaConfiguration={"optionalValue":{"defaultSchema": <same schema with the `critical` property removed>}}).
3. Observe the ValidationException message: the emulator answers 'The custom metadata schema cannot remove field critical from defaultSchema; fields can only be added.' where AWS answers "Cannot remove field 'critical' from record type 'default schema'. Schema evolution is additive-only."
4. Repeat step 2 with a replacement that keeps both fields but narrows environment's enum to ["prod"]: the emulator answers 'The custom metadata schema cannot remove allowed value dev of field environment in defaultSchema; allowed values can only be added.' where AWS answers "Cannot remove enum value 'dev' from field 'environment' in record type 'default schema'."
5. GetRegistry after each refusal — the stored schema is unchanged, which is correct.
