Issue-Source: trace-sweep-divergence
Issue-Trace: b2bi-transformer-job-error-paths
Issue-Step: setup#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: b2bi-transformer-job-error-paths
Issue-Shape-Id: com.amazonaws.b2bi#B2BI
Issue-Key: b2bi|CreateTransformer|2xx->2xx|$.transformerId
Issue-Commit: f468f37ea639de53f535b93618c112f23df311d1

# b2bi CreateTransformer mints transformerId with prefix "t-" instead of AWS's "tr-"

> _Note: this issue was identified against the code base at commit `f468f37ea639de53f535b93618c112f23df311d1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTransformer

## Description
One cause.

Cause 1 of 1: the system-assigned identifier CreateTransformer returns does not follow AWS's transformer-id format, so the id the recording captured as the fixture's transformer_id variable cannot be bound to what the emulator mints.

AWS: CreateTransformer answers HTTP 200 with a required `transformerId` (Smithy: CreateTransformerResponse$transformerId -> com.amazonaws.b2bi#TransformerId, @length min 1 max 64, @pattern ^[a-zA-Z0-9_-]+$, doc "Returns the system-assigned unique identifier for the transformer"). The pattern is permissive, but B2BI's id TEMPLATE per resource type is fixed and is stated by the service's own Smithy @examples: a transformer id is the literal prefix `tr-` followed by 17 lowercase hex digits (model example, CreateCapability sample: `"transformerId": "tr-9a893cf536df4658b"`), exactly as the sibling resources are `ca-<17 hex>` (`ca-963a8121e4fc4e348`), `ps-<17 hex>` (`ps-219fa02f5b4242af8`), `p-<17 hex>` (`p-60fbc37c87f04fce9`) and `tp-<17 hex>` (`tp-2a17ca447f6f4a8a8`). The prefix is the fixed text of that template; only the 17 hex digits are a per-call value.

Emulator: the create succeeds (status 200 == 200) but returns a transformerId built from the prefix `t` rather than `tr` — the replayed step observed `$.transformerId` = `t-<17 hex>` where the fixture's recorded response carries the captured transformer id (`{{transformer_id}}`), so the comparator reports `$.transformerId: value_mismatch`. The id is minted in the CreateTransformer handler as `crate::state::new_resource_id("t")` (see Traces), and the same wrong prefix is then embedded in `transformerArn` (`transformer/<id>`) and in the state key every later read-back uses.

Hypothesis: because the minted id does not match the recorded transformer-id template, the replay harness could not bind its `transformer_id` capture to the emulator's value, which is why the placeholder is reported unresolved at the step that defines it.

Why: clients and tooling treat the resource-type prefix as the stable, documented part of a B2BI id — console/CLI examples, CloudFormation templates, IAM resource patterns and customer code routinely match or validate `tr-*` for transformers (and `ca-*`/`ps-*`/`p-*` for the others). A client that validates or dispatches on the prefix rejects or misroutes an emulator-minted transformer, and any id-shape assertion carried over from a real recording fails even though the call otherwise succeeded. Only the hex suffix is allowed to differ between runs; the prefix is not.

Fix: the id minting for the Transformer resource — the `new_resource_id` prefix used by the CreateTransformer handler, so a transformer id is `tr-` + 17 hex. The change must flow through everything derived from it in the same create (the `transformerArn` resource segment and the state key), so GetTransformer / ListTransformers / DeleteTransformer / UpdateTransformer, StartTransformerJob and the capability `configuration.edi.transformerId` reference all agree on the new form.

Traces: fixture `b2bi-transformer-job-error-paths`, step `setup#1` (CreateTransformer). Emulator code: services/service-com-amazonaws-b2bi-b2bi/src/ops/create_transformer.rs lines 527-529 (`new_resource_id("t")`) and line 537 (`transformer/{transformer_id}` in the ARN); id shape defined in services/service-com-amazonaws-b2bi-b2bi/src/state.rs lines 86-91 (`ID_HEX_DIGITS = 17`).

Not covered by the other open b2bi issues: 20261118T004349Z-b2bi-createstartermappingtemplate-returns-a-starter-template-whose-interchange.md (starter mapping-template element keys) and 20261109T123922Z-b2bi-cloudformation-aws-b2bi-profile-transformer-capability-partnership.md (unregistered CloudFormation resource types) are different faults.

## Evidence
Replayed step `b2bi-transformer-job-error-paths#setup#1` (CreateTransformer), verdict mismatch, expected_status 200 / actual_status 200.

Detail from the replay: `$.transformerId: value_mismatch (expected '{{transformer_id}}', got 't-c829bd5fe06028aa5')`.

Expected (AWS): `transformerId` matching the documented transformer-id template `tr-` + 17 lowercase hex digits — fixed text `tr-`, per-call value the hex suffix. Smithy @examples for this service show `"transformerId": "tr-9a893cf536df4658b"`; siblings `ca-963a8121e4fc4e348`, `ps-219fa02f5b4242af8`, `p-60fbc37c87f04fce9`, `tp-2a17ca447f6f4a8a8`.

Actual (emulator): `transformerId` of the form `t-` + 17 hex (the recorded run observed `t-c829bd5fe06028aa5`; the hex digits are per-run, the `t-` prefix is not). The same prefix propagates into `transformerArn` as `.../transformer/t-<17 hex>`.

No error code or message is involved: both sides answer HTTP 200; only the id's fixed prefix diverges.

## Steps to Reproduce
1. Boot the emulator from this checkout and point a b2bi client at it.
2. Call CreateTransformer with a valid `name` and a modern configuration (e.g. `inputConversion`), as fixture `b2bi-transformer-job-error-paths` step `setup#1` does.
3. Read `transformerId` in the 200 response: it is `t-<17 hex>` where AWS returns `tr-<17 hex>`.
4. Equivalently, replay the wire corpus: fixture `b2bi-transformer-job-error-paths`, step `setup#1` reports `$.transformerId: value_mismatch`.
5. Also observe `transformerArn` from the same response carries the resource segment `transformer/t-<17 hex>`.
