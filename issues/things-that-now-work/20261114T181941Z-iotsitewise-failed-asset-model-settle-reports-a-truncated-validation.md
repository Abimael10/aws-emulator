Issue-Source: trace-sweep-divergence
Issue-Trace: iotsitewise-windfarm-trajwire-raw-measurement-rollup-fails-then-repaired
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iotsitewise-windfarm-trajwire-raw-measurement-rollup-fails-then-repaired
Issue-Shape-Id: com.amazonaws.iotsitewise#AWSIoTSiteWise
Issue-Key: iotsitewise|DescribeAssetModel|2xx->2xx|$.assetModelStatus.error.message
Issue-Commit: bdeab38f08235e16190fe33dc246d4235cdc81f4

# FAILED asset-model settle reports a truncated VALIDATION_ERROR message for a metric referencing a child measurement

> _Note: this issue was identified against the code base at commit `bdeab38f08235e16190fe33dc246d4235cdc81f4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAssetModel

## Description
This issue bundles 1 cause; it must be closed.

Cause 1 of 1: when an asset model is created whose metric property rolls up a NON-metric (measurement/attribute/transform) property of a child model reached through a hierarchy, IoT SiteWise accepts the create and settles the model FAILED; DescribeAssetModel then reports assetModelStatus.error.code = VALIDATION_ERROR with the rule that fired in assetModelStatus.error.message. The emulator settles FAILED and answers 200 with the right error code, but writes its own shortened text for that rule instead of AWS's message.

AWS: Smithy (com.amazonaws.iotsitewise#AWSIoTSiteWise): DescribeAssetModel GET /asset-models/{assetModelId} -> 200 with assetModelStatus @required (AssetModelStatus{state @required: AssetModelState, error: ErrorDetails}); ErrorDetails{code @required: ErrorCode enum [VALIDATION_ERROR, INTERNAL_FAILURE], message @required: ErrorMessage, details}. The model types the diagnosis but not its wording, so the message text is recorded behavior only. The recording shows that for this rule AWS answers 200 with assetModelStatus.error.message = "A metric property references a non-metric property from a child model. Metrics can reference properties from the current model and metric properties from child models." That string is entirely FIXED text — two sentences, the second stating the general rule — with NO variable parts: no property name, property id, model id or hierarchy id is interpolated. (Note the sibling rule for a reference to a child property that does not exist at all has its own distinct fixed text, "A formula property references a child asset model property that doesn't exist."; the two must not be collapsed into one string.)

Emulator: the same DescribeAssetModel answers 200 with assetModelStatus.error.message = "A formula property references a non-metric property from a child model." — a single sentence that (a) says "formula property" where AWS says "metric property" and (b) drops AWS's second sentence "Metrics can reference properties from the current model and metric properties from child models." entirely. Status (200), the state (FAILED) and error.code (VALIDATION_ERROR) all match; only the message text diverges.

Why: the FAILED settle's error.message is the only machine- and human-readable statement of WHICH validation rule rejected the model — a client polling DescribeAssetModel after an accepted create matches on, logs, or surfaces that text to distinguish "metric points at a non-metric child property" from "metric points at a child property that doesn't exist". Because the emulator's wording overlaps the neighbouring rule's wording ("formula property references ...") and omits the remediation sentence, such a client mis-classifies the failure or finds no match at all, and operator-facing output differs from production.

Fix: the asset-model settle/transition path that composes ErrorDetails for a FAILED model — the validation-rule-to-message table used when writing assetModelStatus.error, read back by the DescribeAssetModel handler's serializer — must carry AWS's full fixed text for this rule verbatim, keeping it distinct from the child-property-does-not-exist rule's text. No request or state value is interpolated into either message.

Related open issues (neither covers this message): 20261103T025010Z-iotsitewise-invalid-asset-model-definitions-settle-active-instead-of.md covers models that never settle FAILED at all, and 20261110T184924Z-iotsitewise-createassetmodel-refuses-an-out-of-hierarchy-metric-variable.md covers a synchronous refusal of a different rule; here the model does settle FAILED and only the message text is wrong.

Traces: iotsitewise-windfarm-trajwire-raw-measurement-rollup-fails-then-repaired#task#4.

## Evidence
Wire corpus simparity-wire-aws-v1 replayed against the emulator built from this checkout: 147/171 steps passed, 21 mismatched. Cluster service=iotsitewise|DescribeAssetModel|status=200->200|keys=$.assetModelStatus.error.message, fixture iotsitewise-windfarm-trajwire-raw-measurement-rollup-fails-then-repaired, step task#4, affected_count 1, owned_by: none.

Expected (recorded AWS): HTTP 200, $.assetModelStatus.error.message = "A metric property references a non-metric property from a child model. Metrics can reference properties from the current model and metric properties from child models."
Actual (emulator): HTTP 200, $.assetModelStatus.error.message = "A formula property references a non-metric property from a child model."
Diff reported by the replay: "$.assetModelStatus.error.message: value_mismatch". All other asserted fields of the response, including assetModelStatus.state and assetModelStatus.error.code (VALIDATION_ERROR), matched.

Message anatomy: the expected message is 100% fixed text; nothing in it comes from the request or from emulator state, so the divergence is not a per-run value difference.

## Steps to Reproduce
1. POST /asset-models creating a child asset model that exposes a measurement property (e.g. a raw DOUBLE measurement) and wait for it to become ACTIVE.
2. POST /asset-models creating a parent model that declares a hierarchy whose childAssetModelId is that child, and a metric property whose variable targets the child's MEASUREMENT property (a non-metric) through that hierarchyId.
3. The create is accepted (2xx, state CREATING) and the model settles FAILED.
4. GET /asset-models/{parentId} and read assetModelStatus.error. Observe code = VALIDATION_ERROR but message = "A formula property references a non-metric property from a child model." instead of AWS's "A metric property references a non-metric property from a child model. Metrics can reference properties from the current model and metric properties from child models."
5. Re-replay fixture iotsitewise-windfarm-trajwire-raw-measurement-rollup-fails-then-repaired step task#4 to confirm the fix.
