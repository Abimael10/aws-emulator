Issue-Source: trace-sweep-divergence
Issue-Trace: config-list-evaluations-by-context-identifier
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: config-list-evaluations-by-context-identifier config-proactive-evaluation-lifecycle config-proactive-resource-evaluation-lifecycle config-resource-evaluation-context-grouping config-resource-evaluation-grouped-by-context config-start-resource-evaluation-smoke
Issue-Shape-Id: com.amazonaws.configservice#StarlingDoveService
Issue-Key: config|StartResourceEvaluation|2xx->2xx|$.ResourceEvaluationId
Issue-Commit: 748a8a1bc704f94a0a2a8328e7bfeac677ed6e66

# config StartResourceEvaluation mints a ResourceEvaluationId whose form the recorded responses do not carry

> _Note: this issue was identified against the code base at commit `748a8a1bc704f94a0a2a8328e7bfeac677ed6e66`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartResourceEvaluation

## Description
Cause 1 of 1: StartResourceEvaluation answers 200 but the ResourceEvaluationId it assigns is not of the form AWS Config assigns, so the identifier the whole proactive-evaluation surface is keyed by is shaped wrongly on every start.

AWS: recorded behavior only. The Smithy model (services/service-com-amazonaws-configservice-starlingdoveservice/smithy-model/com-amazonaws-configservice-starlingdoveservice.json) constrains StartResourceEvaluation's output member only as `ResourceEvaluationId` (@length min 1, max 128) with no @pattern, and documents it as "A unique ResourceEvaluationId that is associated with a single execution." The requirement therefore comes from the recording: every recorded StartResourceEvaluation 200 body carries a ResourceEvaluationId of one specific token form, and the fixture asserts that form (the value itself is per-run and is recorded as `<shaped-not-asserted>`, i.e. shape-asserted, value-free). The emulator's token does not belong to that form's shape class, which is what the replay reports. Hypothesis: AWS Config assigns a hyphen-delimited UUID-style id here (as its API-reference examples for resource evaluations show), not an unbroken alphanumeric run; the authority for the fix is the recorded form in the fixtures above, not this hypothesis.

Emulator: `crate::state::mint_resource_evaluation_id()` draws the id as ONE unbroken run of base-36 characters, 16 of them (`RESOURCE_EVALUATION_ID_CHARS: usize = 16`), e.g. a token of the shape `5f7q1s9gjy6xi6ry` (that literal value is one run's draw, not a requirement). The in-tree doc comment on that function asserts "the recorded ids are one compact opaque token with no hyphen-delimited groups" — the replay contradicts exactly that premise, so the comment is the defect's root: the id generator was written to a form the corpus does not record. Status and body are otherwise correct (200, single member present); the only divergence is the token's form. Replay verdict: mismatch, expected_status 200 / actual_status 200, `$.ResourceEvaluationId: value_mismatch (expected '<shaped-not-asserted>', got '5f7q1s9gjy6xi6ry')`.

Why: a client that validates, parses or stores a ResourceEvaluationId on AWS's form gets it wrong here — an SDK or tool that round-trips the id into GetResourceEvaluationSummary / ListResourceEvaluations / GetResourceEvaluationSummary-keyed records, or that pattern-matches it to tell a resource-evaluation id from another Config identifier, or that sizes a column/field for AWS's id width, sees an id AWS would never issue. Because every proactive-evaluation read path is keyed by this id, the wrong form propagates from the very first call of the lifecycle into every later response that echoes it, so no recording of that lifecycle can be reproduced byte-for-byte even when the rest of the surface is right.

Fix: the id generator in this service's state (`mint_resource_evaluation_id` / `RESOURCE_EVALUATION_ID_CHARS`, which is what StartResourceEvaluation's handler calls to assign the id before `record_resource_evaluation`). Re-derive the token form from the recorded responses and draw the id in that form, and correct the doc comment that justifies the current unbroken base-36 run. Nothing else in the handler changes: validation, idempotency by ClientToken and the persisted entry are all unaffected.

Traces: config-list-evaluations-by-context-identifier#task#1 (representative); the same cause also fails config-list-evaluations-by-context-identifier#task#2, config-proactive-evaluation-lifecycle#task#1, config-proactive-resource-evaluation-lifecycle#task#1, config-resource-evaluation-context-grouping#task#1, config-resource-evaluation-context-grouping#task#2, config-resource-evaluation-grouped-by-context#task#1, config-resource-evaluation-grouped-by-context#task#3 and config-start-resource-evaluation-smoke#task#1 — 9 steps in one cluster, all the same divergence_key `service=config|StartResourceEvaluation|status=200->200|keys=$.ResourceEvaluationId`.

Not covered by any open issue: the only open config conformance/evaluation issues are 20261124T184352Z-config-reopens-config-conformance-pack-deployment-has-no-failure.md (conformance-pack deployment state, the other cluster in this replay), 20261122T033830Z-config-sqs-a-queue-created-under-a-recording.md and 20261122T162354Z-config-sqs-a-configuration-recorder-restarted-with-startconfigurationrecorder.md (configuration-recorder recording). None of them touches resource-evaluation id minting.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 282 steps, 269 passed, score 95.4%).

Cluster (verdict=mismatch, service=config, operation=StartResourceEvaluation, affected_count=9):
  fixture_id: config-list-evaluations-by-context-identifier
  step_key:   task#1
  expected_status: 200   actual_status: 200
  detail: $.ResourceEvaluationId: value_mismatch (expected '<shaped-not-asserted>', got '5f7q1s9gjy6xi6ry')
  divergence_key: service=config|StartResourceEvaluation|status=200->200|keys=$.ResourceEvaluationId
  affected: config-list-evaluations-by-context-identifier#task#1, config-list-evaluations-by-context-identifier#task#2, config-proactive-evaluation-lifecycle#task#1, config-proactive-resource-evaluation-lifecycle#task#1, config-resource-evaluation-context-grouping#task#1, config-resource-evaluation-context-grouping#task#2, config-resource-evaluation-grouped-by-context#task#1, config-resource-evaluation-grouped-by-context#task#3, config-start-resource-evaluation-smoke#task#1

Expected: a 200 whose ResourceEvaluationId is of the token form every recorded StartResourceEvaluation response carries (shape asserted, value per-run).
Actual: a 200 whose ResourceEvaluationId is a 16-character unbroken base-36 token, a different shape class.

Model evidence (smithy_query, operation=StartResourceEvaluation): output `{"ResourceEvaluationId": {"target": "ResourceEvaluationId", "length": {"min": 1, "max": 128}}}` — length only, no pattern, so the form is fixed by the recording, not the model.

Source evidence: services/service-com-amazonaws-configservice-starlingdoveservice/src/ops/start_resource_evaluation.rs:275 calls `crate::state::mint_resource_evaluation_id()`; services/service-com-amazonaws-configservice-starlingdoveservice/src/state.rs:4258-4274 defines `RESOURCE_EVALUATION_ID_CHARS = 16` and `mint_resource_evaluation_id() -> opaque_id(RESOURCE_EVALUATION_ID_CHARS)`, with a comment asserting "the recorded ids are one compact opaque token with no hyphen-delimited groups".

## Steps to Reproduce
1. git clone the repo and build the emulator from this checkout.
2. Replay the simparity-wire-aws-v1 wire corpus against it.
3. Fixture config-start-resource-evaluation-smoke, step task#1 (or config-list-evaluations-by-context-identifier#task#1, the cluster representative) POSTs StarlingDoveService.StartResourceEvaluation with EvaluationMode=PROACTIVE and a JSON-object ResourceDetails.ResourceConfiguration.
4. The emulator answers 200 with ResourceEvaluationId set to a 16-character unbroken base-36 token (e.g. '5f7q1s9gjy6xi6ry').
5. The step's shape assertion on $.ResourceEvaluationId fails: the recorded id form is a different shape class. Equivalent manual check: `aws configservice start-resource-evaluation --evaluation-mode PROACTIVE --resource-details '{"ResourceId":"probe","ResourceType":"AWS::S3::Bucket","ResourceConfiguration":"{}"}'` against the emulator and compare the id's form with a recorded AWS response.
