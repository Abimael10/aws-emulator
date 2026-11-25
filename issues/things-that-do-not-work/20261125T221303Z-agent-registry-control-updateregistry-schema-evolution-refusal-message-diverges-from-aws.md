Issue-Source: trace-sweep-divergence
Issue-Trace: agent-registry-control-custom-metadata-schema-enforcement
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: agent-registry-control-custom-metadata-schema-enforcement
Issue-Shape-Id: com.amazonaws.agentregistrycontrol#AgentRegistryControl
Issue-Key: agent-registry-control|UpdateRegistry|4xx->4xx|$.message
Issue-Commit: 5de8975540ca22ec41f789488a788a9a12b974db

# UpdateRegistry schema-evolution refusal message diverges from AWS wording

> _Note: this issue was identified against the code base at commit `5de8975540ca22ec41f789488a788a9a12b974db`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateRegistry

## Description
One root cause, one net-new cause in this issue.

Cause 1 of 1 (fixture agent-registry-control-custom-metadata-schema-enforcement, step task#5): the ValidationException message the emulator composes when an UpdateRegistry custom-metadata schema replacement WITHDRAWS something is written in the emulator's own sentences instead of the sentence AWS words for that error class. The status (400) and the error code (ValidationException) are correct; only the message text diverges.

AWS: UpdateRegistry (PATCH /registries/{registryId}, success 202) models ValidationException with @httpError 400, and documents customMetadataSchemaConfiguration (target UpdatedCustomMetadataSchemaConfiguration) as a full replacement in which fields and allowed values may be ADDED but never removed and a field's type never changed (Smithy: UpdateRegistry.customMetadataSchemaConfiguration -> UpdatedCustomMetadataSchemaConfiguration; errors AccessDeniedException 403, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ServiceQuotaExceededException 402, ThrottlingException 429, ValidationException 400). The model does not state the refusal TEXT, so the required wording below is recorded behavior only, taken from the fixture's recorded response:
  - a field dropped from a scope -> "Cannot remove field '<field>' from record type '<scope>'. Schema evolution is additive-only." FIXED text: "Cannot remove field '", "' from record type '", "'. Schema evolution is additive-only." VARIABLE parts taken from the request and the stored schema: <field> is the path of the field the current schema declares and the replacement no longer declares, single-quoted; <scope> is the scope the schema is carried in, and for the configuration's defaultSchema member AWS names that scope "default schema" (two words, lower case, single-quoted) — NOT the wire member name "defaultSchema".
  - an enumerated value dropped from a field -> "Cannot remove enum value '<value>' from field '<field>' in record type '<scope>'." FIXED text: "Cannot remove enum value '", "' from field '", "' in record type '", "'." VARIABLE parts: the dropped enumerated value, the field path and the scope, each single-quoted; AWS words this one with NO trailing additive-only sentence.
The recorded field name 'critical' and scope value in the fixture are request/state values filling that template, not part of it.

Emulator: for the field-withdrawal case it answers HTTP 400 ValidationException with message "The custom metadata schema cannot remove field critical from defaultSchema; fields can only be added." where the recording expects "Cannot remove field 'critical' from record type 'default schema'. Schema evolution is additive-only." — a different lead-in, the wire member name defaultSchema where AWS says record type 'default schema', no single quotes around the identifier, and a "; fields can only be added." tail in place of ". Schema evolution is additive-only.". The same composer words the allowed-value withdrawal "The custom metadata schema cannot remove allowed value <value> of field <field> in <scope>; allowed values can only be added.", diverging from AWS's "Cannot remove enum value '<value>' from field '<field>' in record type '<scope>'." in the same way, and the type-change withdrawal is composed by the same function.

Why: a client relying on AWS's behavior — a deploy tool surfacing why a schema change was rejected, a test asserting the rejection reason, or any code that parses the withdrawn field name or value back out of the sentence — is given text that does not match AWS. The single quotes AWS puts around the field name and the value are what make those identifiers extractable, and the emulator leaves them unquoted, so a parser keyed on AWS's wording recovers nothing and the caller is told the update failed without being told, in AWS's terms, what the replacement withdrew.

Fix: the refusal-wording helper in this service's state module that every schema-evolution withdrawal message funnels through, including how it labels a scope (the defaultSchema scope must render as AWS's "default schema") and the single-quoting of identifiers. The detection logic is right — it picks the correct withdrawal and the correct field — and the handler's status, error shape and its "refuse before the registry is written" ordering are right and must not change.

Note: open issue 20261125T071516Z-agent-registry-control-updateregistry-custom-metadata-schema-evolution-refusals-are-worded.md describes the same wording fault from the BEHAVIORAL corpus, but it carries no wire replay key (its Issue-Corpus is simparity-behavioral-aws-v1 and its Issue-Trace-Ids is a check id), so it does not gate this fixture step; this issue supplies the wire linkage.

Traces: agent-registry-control-custom-metadata-schema-enforcement#task#5 and #task#6 (same cluster, same composer). Relevant source: services/service-com-amazonaws-agentregistrycontrol-agentregistrycontrol/src/state/schema_evolution.rs lines 75-86 (Scope::label) and 242-261 (message); call site services/service-com-amazonaws-agentregistrycontrol-agentregistrycontrol/src/ops/update_registry.rs.

## Evidence
Fixture agent-registry-control-custom-metadata-schema-enforcement, step task#5 (cluster also covers task#6), verdict mismatch.

divergence_key: service=agent-registry-control|UpdateRegistry|status=400->400|keys=$.message
Expected status 400, actual status 400 — status and Error.Code (ValidationException) agree.
$.message: value_mismatch
  Expected (recorded AWS): "Cannot remove field 'critical' from record type 'default schema'. Schema evolution is additive-only."
  Actual   (emulator):     "The custom metadata schema cannot remove field critical from defaultSchema; fields can only be added."

Fixed text of the expected message: "Cannot remove field '" + "' from record type '" + "'. Schema evolution is additive-only.". Variable parts: the withdrawn field path (here 'critical', taken from the comparison of the stored and replacement schemas) and the scope label (here 'default schema', the AWS name for the configuration's defaultSchema member).

Not diverging and to be left alone: the HTTP 400 status, the ValidationException error code, and the registry's stored schema remaining unchanged after the refusal.

## Steps to Reproduce
1. Replay fixture agent-registry-control-custom-metadata-schema-enforcement against the emulator.
2. The fixture creates a registry whose customMetadataSchemaConfiguration.defaultSchema declares fields environment (string with an enum) and critical (boolean), and waits for status READY.
3. At step task#5 it calls UpdateRegistry(registryId=<id>, customMetadataSchemaConfiguration={"optionalValue":{"defaultSchema": <same schema with the critical property removed>}}).
4. Compare the ValidationException Error.Message: the emulator answers "The custom metadata schema cannot remove field critical from defaultSchema; fields can only be added." where the recording expects "Cannot remove field 'critical' from record type 'default schema'. Schema evolution is additive-only.". Step task#6 diverges identically through the same composer.
5. GetRegistry afterwards — the stored schema is unchanged, which is correct.
