Issue-Source: trace-sweep-divergence
Issue-Trace: drs-recovery-plan-lifecycle-and-tag-round-trip
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: drs-recovery-plan-lifecycle-and-tag-round-trip
Issue-Shape-Id: com.amazonaws.drs#ElasticDisasterRecoveryService
Issue-Key: drs|stub:ListTagsForResource
Issue-Commit: 656cbf9b11e99225d4fd85cb8b7394b019211c7b

# Implement drs ListTagsForResource: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `656cbf9b11e99225d4fd85cb8b7394b019211c7b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
`ListTagsForResource` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/src/ops/list_tags_for_resource.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

2 recorded unit(s) fail on their `ListTagsForResource` call, so each one unblocks only when `ListTagsForResource` works: `drs-recovery-plan-lifecycle-and-tag-round-trip#task#5`, `drs-recovery-plan-lifecycle-and-tag-round-trip#task#7`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/src/ops/list_tags_for_resource.rs`. Replaying the corpus, the emulator refused the `ListTagsForResource` call: status: expected 403, got 501
$.message: value_mismatch (expected 'Unable to determine service/operation name to be authorized', got 'operation POST /ListTagsForResource is not implemented in this emulator')
$headers.x-amzn-errortype: shape_mismatch (expected '<shaped-not-asserted>', got 'NotImplemented')

## Steps to Reproduce
Boot the drs emulator
Call ListTagsForResource and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
