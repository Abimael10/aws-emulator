Issue-Source: trace-sweep-divergence
Issue-Trace: osis-validate-pipeline-configs
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: osis-validate-pipeline-configs
Issue-Shape-Id: com.amazonaws.osis#AmazonOpenSearchIngestionService
Issue-Key: osis|ValidatePipeline|2xx->2xx|$.isValid
Issue-Key: osis|ValidatePipeline|2xx->2xx|$.Errors
Issue-Commit: 584d32f997b3f53f5f104457b63df8dc81cb1e93

# osis ValidatePipeline reports the wrong isValid verdict: it judges only sts_role_arn assumability and never the body's structure

> _Note: this issue was identified against the code base at commit `584d32f997b3f53f5f104457b63df8dc81cb1e93`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ValidatePipeline

## Description
`ValidatePipeline` is the pre-flight gate a client asks before `CreatePipeline`/`UpdatePipeline`, and the emulator answers the WRONG verdict in BOTH directions, because the only thing its judgement looks at is whether every `sts_role_arn` the document names resolves to a locally stored, assumable IAM role. A well-formed configuration is therefore declared invalid, and a body that is not a Data Prepper configuration at all is declared valid. 2 causes, both on ValidatePipeline, both reproduced by fixture `osis-validate-pipeline-configs`.

Cause 1 of 2: a VALID configuration body is reported `isValid: false` (fixture `osis-validate-pipeline-configs`, step `task#1`).
AWS: `ValidatePipeline` (POST /2022-01-01/osis/validatePipeline, success 200) returns `isValid` — "A boolean indicating whether or not the pipeline configuration is valid" — and `Errors` — "A list of errors if the configuration is invalid" (Smithy: ValidatePipelineRequest.PipelineConfigurationBody @length 1..100000 required; ValidatePipelineResponse {isValid: Boolean, Errors: ValidationMessageList}; modeled errors AccessDeniedException 403, DisabledOperationException 409, InternalException 500, ValidationException 400). The model states no rule making a configuration's verdict depend on IAM state, so for the submitted well-formed body this is recorded behavior only: the recording shows HTTP 200 with `isValid: true` and no `Errors` list for a body that was never accompanied by any role creation in this account.
Emulator: HTTP 200 with `isValid: false` (and a populated `Errors` list of role-assumability messages). The judgement is `crate::state::configuration_validation_messages`, which emits `Unable to assume the STS role <roleArn>. This role must contain a trust relationship with 'osis-pipelines.amazonaws.com'.` for every `sts_role_arn` in the document that the local IAM store does not hold with a matching trust policy. Hypothesis: the fixture's body names ordinary example role ARNs that no caller created in the emulator, so every one of them is judged unassumable and the body is reported invalid.
Why: a client that gates on `isValid` never submits a configuration AWS accepts — the whole create flow stalls on a false negative — and because `CreatePipeline`/`UpdatePipeline` share this judgement (`unassumable_configuration_roles`), the same body is also refused at create with a 400 ValidationException the real service does not return.
Fix: the operation's validation — the configuration judgement shared by the handler and the submit paths — must not make a valid verdict conditional on a role being present in local IAM state; an unresolvable role in the emulator's store is not evidence that the configuration is invalid.
Traces: osis-validate-pipeline-configs#task#1.

Cause 2 of 2: a body that is not a pipeline configuration is reported `isValid: true` with no `Errors` (fixture `osis-validate-pipeline-configs`, step `task#2`).
AWS: the structural reading of `PipelineConfigurationBody` is part of the verdict: the recording shows HTTP 200 with `isValid: false` and `Errors` carrying one `ValidationMessage` whose `Message` is the Data Prepper deserialization failure — fixed text `pipelineConfigurationBody structure is invalid: Cannot deserialize value of type \`org.opensearch.dataprepper.model.configuration.PipelineModel\` from Array value (token \`JsonToken.START_ARRAY\`)` followed by the source location and the reference chain `org.opensearch.dataprepper.model.configuration.PipelinesDataFlowModel["<topLevelKey>"]`; the variable parts are the source line/column of the offending node and `<topLevelKey>`, the top-level pipeline key taken from the submitted document (in the recording, `not-a-pipeline`). The status stays 200 — an unparseable body is a reported verdict, not a modeled error. Beyond that recording the model says nothing about the wording, so: recorded behavior only.
Emulator: HTTP 200 with `isValid: true` and `Errors` absent. `src/ops/validate_pipeline.rs` documents this gap explicitly ("The SYNTAX of a configuration body is not judged here"), and `configuration_role_arns` returns an empty role list for a body that does not parse — so a body that parses to nothing to judge is reported as having nothing wrong with it.
Why: a client uses this call to catch a malformed configuration before creating a pipeline; a false positive sends a body AWS rejects straight into `CreatePipeline`, and the caller gets no validation message to show a user.
Fix: the operation's validation — the shared configuration judgement must read the body as a Data Prepper document and raise a structural `ValidationMessage` when it does not deserialize, before (and in addition to) the role questions, so `isValid` reflects the body's structure.
Traces: osis-validate-pipeline-configs#task#2.

Related open issue: `20261021T234042Z-osis-validatepipeline-stub.md` ("Implement osis ValidatePipeline: unimplemented scaffold stub") is a behavioral-corpus issue about the operation answering 501; the operation now answers 200 and carries a handler, and that issue carries no wire replay key, so it does NOT cover either cause above.

## Evidence
Fixture `osis-validate-pipeline-configs`, step `task#1` (ValidatePipeline, verdict mismatch, expected status 200, actual status 200):
  `$.isValid: value_mismatch (expected True, got False)`
  Expected: 200 `{"isValid": true}` with no `Errors`.
  Actual: 200 `isValid: false`, with `Errors` listing one `Unable to assume the STS role <roleArn>. This role must contain a trust relationship with 'osis-pipelines.amazonaws.com'.` per role named in the body.

Fixture `osis-validate-pipeline-configs`, step `task#2` (ValidatePipeline, verdict mismatch, expected status 200, actual status 200):
  `$.Errors: missing (expected [{'Message': 'pipelineConfigurationBody structure is invalid: Cannot deserialize value of type `org.opensearch.dataprepper.model.configuration.PipelineModel` from Array value (token `JsonToken.START_ARRAY`)\n at [Source: (StringReader); line: 1, column: 17] (through reference chain: org.opensearch.dataprepper.model.configuration.PipelinesDataFlowModel["not-a-pipeline"])'}], got None)`
  `$.isValid: value_mismatch (expected False, got True)`
  Fixed text of that message: `pipelineConfigurationBody structure is invalid: Cannot deserialize value of type `org.opensearch.dataprepper.model.configuration.PipelineModel` from Array value (token `JsonToken.START_ARRAY`)` plus the ` at [Source: ...; line: <n>, column: <n>] (through reference chain: org.opensearch.dataprepper.model.configuration.PipelinesDataFlowModel["<topLevelKey>"])` suffix. Variable parts: the line/column of the offending node and `<topLevelKey>` from the request body (`not-a-pipeline` in this recording). Everything else is fixed.

Code: `services/service-com-amazonaws-osis-amazonopensearchingestionservice/src/ops/validate_pipeline.rs:160-188` (the whole verdict is `configuration_validation_messages`); `services/service-com-amazonaws-osis-amazonopensearchingestionservice/src/state/configuration_roles.rs:46-58` (a body that does not parse names no role) and `:139-158` (the only messages raised are role-assumability ones).

## Steps to Reproduce
1. Boot the emulator and replay fixture `osis-validate-pipeline-configs`.
2. Step `task#1`: POST /2022-01-01/osis/validatePipeline with the fixture's well-formed Data Prepper configuration body (one naming `sts_role_arn` values for which no role was created in the emulator). Observe 200 with `isValid: false` and an `Errors` list of `Unable to assume the STS role ...` messages, where the recording has `isValid: true` and no `Errors`.
3. Step `task#2`: POST /2022-01-01/osis/validatePipeline with a body that is not a Data Prepper configuration (a top-level key whose value is a YAML/JSON array, e.g. `not-a-pipeline: [...]`). Observe 200 with `isValid: true` and no `Errors`, where the recording has `isValid: false` and one `ValidationMessage` reporting `pipelineConfigurationBody structure is invalid: Cannot deserialize value of type ...PipelineModel from Array value ...`.
